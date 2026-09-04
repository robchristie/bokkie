//! Reusable supervision for child processes.
//!
//! This is the single owner of child lifetime, process-group termination,
//! heartbeat cadence, absolute deadlines, cancellation and bounded output.
//! Adapters retain responsibility for interpreting exit status and protocol
//! messages, and lifecycle transitions remain Store-owned.

use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

const READ_CHUNK_BYTES: usize = 8 * 1024;

/// A clonable cancellation signal shared by daemon shutdown and child sessions.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Limits applied while a child is running. They do not depend on lease length.
#[derive(Clone, Copy, Debug)]
pub struct ProcessLimits {
    pub stdin_message_bytes: usize,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub jsonl_line_bytes: usize,
    pub final_message_bytes: usize,
    pub termination_grace: Duration,
    pub poll_interval: Duration,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            stdin_message_bytes: 1024 * 1024,
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 256 * 1024,
            jsonl_line_bytes: 256 * 1024,
            final_message_bytes: 256 * 1024,
            termination_grace: Duration::from_millis(250),
            poll_interval: Duration::from_millis(25),
        }
    }
}

/// Whether interruption could leave an effect whose result must be reconciled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectRisk {
    None,
    AmbiguousOnInterruption,
}

/// The reason supervision stopped a process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Interruption {
    Timeout,
    Cancellation,
    OutputLimit { stream: &'static str, limit: usize },
    HeartbeatFailure { message: String },
}

/// Tail-retaining evidence for one output stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OutputEvidence {
    pub total_bytes: u64,
    pub retained_bytes: usize,
    pub sha256: String,
    pub truncated: bool,
    pub tail: String,
    #[serde(skip)]
    pub tail_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessEvidence {
    pub stdout: OutputEvidence,
    pub stderr: OutputEvidence,
}

impl fmt::Display for ProcessEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        evidence_summary(self).fmt(formatter)
    }
}

/// A terminal, typed child outcome.
#[derive(Clone, Debug)]
pub enum ProcessOutcome {
    Completed {
        status: ExitStatus,
        evidence: ProcessEvidence,
    },
    TimedOut(ProcessEvidence),
    Cancelled(ProcessEvidence),
    OutputLimit {
        stream: &'static str,
        limit: usize,
        evidence: ProcessEvidence,
    },
    HeartbeatFailure {
        message: String,
        evidence: ProcessEvidence,
    },
    AmbiguousExternalState {
        cause: Interruption,
        evidence: ProcessEvidence,
    },
}

impl ProcessOutcome {
    pub fn evidence(&self) -> &ProcessEvidence {
        match self {
            Self::Completed { evidence, .. }
            | Self::TimedOut(evidence)
            | Self::Cancelled(evidence)
            | Self::OutputLimit { evidence, .. }
            | Self::HeartbeatFailure { evidence, .. }
            | Self::AmbiguousExternalState { evidence, .. } => evidence,
        }
    }
}

impl fmt::Display for ProcessOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed { status, evidence } => {
                write!(
                    formatter,
                    "completed with {status}; {}",
                    evidence_summary(evidence)
                )
            }
            Self::TimedOut(evidence) => {
                write!(formatter, "timed out; {}", evidence_summary(evidence))
            }
            Self::Cancelled(evidence) => {
                write!(formatter, "cancelled; {}", evidence_summary(evidence))
            }
            Self::OutputLimit {
                stream,
                limit,
                evidence,
            } => write!(
                formatter,
                "{stream} exceeded {limit} bytes; {}",
                evidence_summary(evidence)
            ),
            Self::HeartbeatFailure { message, evidence } => write!(
                formatter,
                "heartbeat failed: {message}; {}",
                evidence_summary(evidence)
            ),
            Self::AmbiguousExternalState { cause, evidence } => write!(
                formatter,
                "external state is ambiguous after {cause:?}; {}",
                evidence_summary(evidence)
            ),
        }
    }
}

fn evidence_summary(evidence: &ProcessEvidence) -> String {
    format!(
        "stdout={} bytes sha256={} truncated={} tail={:?}; stderr={} bytes sha256={} truncated={} tail={:?}",
        evidence.stdout.total_bytes,
        evidence.stdout.sha256,
        evidence.stdout.truncated,
        display_tail(&evidence.stdout.tail),
        evidence.stderr.total_bytes,
        evidence.stderr.sha256,
        evidence.stderr.truncated,
        display_tail(&evidence.stderr.tail),
    )
}

fn display_tail(value: &str) -> &str {
    const DISPLAY_BYTES: usize = 4 * 1024;
    if value.len() <= DISPLAY_BYTES {
        return value;
    }
    let mut start = value.len() - DISPLAY_BYTES;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("cannot spawn child process: {0}")]
    Spawn(#[source] io::Error),
    #[error("child process I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("child I/O worker panicked")]
    IoWorkerPanicked,
}

/// Heartbeat callback invoked synchronously outside Store transactions.
pub trait ProcessHeartbeat {
    fn heartbeat(&mut self) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct NoopHeartbeat;

impl ProcessHeartbeat for NoopHeartbeat {
    fn heartbeat(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ProcessSupervisor {
    heartbeat_interval: Duration,
    limits: ProcessLimits,
    cancellation: CancellationToken,
}

impl ProcessSupervisor {
    pub fn new(
        heartbeat_interval: Duration,
        limits: ProcessLimits,
        cancellation: CancellationToken,
    ) -> Result<Self, String> {
        if heartbeat_interval.is_zero() {
            return Err("heartbeat interval must be positive".to_owned());
        }
        if limits.stdin_message_bytes == 0
            || limits.stdout_bytes == 0
            || limits.stderr_bytes == 0
            || limits.jsonl_line_bytes == 0
            || limits.final_message_bytes == 0
            || limits.poll_interval.is_zero()
        {
            return Err("process output and polling limits must be positive".to_owned());
        }
        let now = Instant::now();
        if now.checked_add(heartbeat_interval).is_none()
            || now.checked_add(limits.termination_grace).is_none()
        {
            return Err("process supervision duration is out of range".to_owned());
        }
        Ok(Self {
            heartbeat_interval,
            limits,
            cancellation,
        })
    }

    pub fn limits(&self) -> ProcessLimits {
        self.limits
    }

    pub fn spawn(
        &self,
        command: &mut Command,
        deadline: Instant,
        risk: EffectRisk,
    ) -> Result<SupervisedChild, ProcessError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.process_group(0);
        let mut child = command.spawn().map_err(ProcessError::Spawn)?;
        let stdin = child.stdin.take().expect("piped stdin is available");
        let stdout = child.stdout.take().expect("piped stdout is available");
        let stderr = child.stderr.take().expect("piped stderr is available");
        let (sender, receiver) = mpsc::sync_channel(32);
        let stdout_reader = spawn_reader(stdout, Stream::Stdout, sender.clone());
        let stderr_reader = spawn_reader(stderr, Stream::Stderr, sender);
        let (stdin_sender, stdin_receiver) = mpsc::sync_channel(1);
        let (writer_sender, writer_receiver) = mpsc::sync_channel(1);
        let stdin_writer = spawn_writer(stdin, stdin_receiver, writer_sender);
        Ok(SupervisedChild {
            child: Some(child),
            stdin_sender: Some(stdin_sender),
            writer_receiver,
            stdin_writer: Some(stdin_writer),
            receiver,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            stdout: Capture::new(self.limits.stdout_bytes),
            stderr: Capture::new(self.limits.stderr_bytes),
            stdout_line: Vec::new(),
            stdout_lines: VecDeque::new(),
            stdout_closed: false,
            stderr_closed: false,
            deadline,
            next_heartbeat: Instant::now()
                .checked_add(self.heartbeat_interval)
                .expect("heartbeat interval was validated"),
            heartbeat_interval: self.heartbeat_interval,
            limits: self.limits,
            cancellation: self.cancellation.clone(),
            risk,
            terminal: None,
        })
    }

    pub fn run(
        &self,
        command: &mut Command,
        deadline: Instant,
        risk: EffectRisk,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ProcessOutcome, ProcessError> {
        let mut child = self.spawn(command, deadline, risk)?;
        child.close_stdin();
        child.wait(heartbeat)
    }
}

#[derive(Debug)]
pub enum JsonlReceive {
    Line(String),
    Eof,
    Terminal(ProcessOutcome),
}

pub struct SupervisedChild {
    child: Option<Child>,
    stdin_sender: Option<SyncSender<Vec<u8>>>,
    writer_receiver: Receiver<io::Result<()>>,
    stdin_writer: Option<JoinHandle<()>>,
    receiver: Receiver<ReaderEvent>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    stdout: Capture,
    stderr: Capture,
    stdout_line: Vec<u8>,
    stdout_lines: VecDeque<Vec<u8>>,
    stdout_closed: bool,
    stderr_closed: bool,
    deadline: Instant,
    next_heartbeat: Instant,
    heartbeat_interval: Duration,
    limits: ProcessLimits,
    cancellation: CancellationToken,
    risk: EffectRisk,
    terminal: Option<Interruption>,
}

impl SupervisedChild {
    pub fn write_json(
        &mut self,
        value: &serde_json::Value,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Option<ProcessOutcome>, ProcessError> {
        let mut message = BoundedMessage::new(self.limits.stdin_message_bytes);
        if let Err(error) = serde_json::to_writer(&mut message, value) {
            if message.exceeded {
                return self
                    .interrupt(Interruption::OutputLimit {
                        stream: "stdin JSONL message",
                        limit: self.limits.stdin_message_bytes,
                    })
                    .map(Some);
            }
            return Err(ProcessError::Io(io::Error::other(error)));
        }
        if message.bytes.len() == self.limits.stdin_message_bytes {
            return self
                .interrupt(Interruption::OutputLimit {
                    stream: "stdin JSONL message",
                    limit: self.limits.stdin_message_bytes,
                })
                .map(Some);
        }
        message.bytes.push(b'\n');
        self.stdin_sender
            .as_ref()
            .ok_or_else(|| {
                ProcessError::Io(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "child stdin is closed",
                ))
            })?
            .try_send(message.bytes)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => ProcessError::Io(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "a child stdin write is already pending",
                )),
                mpsc::TrySendError::Disconnected(_) => ProcessError::Io(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "child stdin writer stopped",
                )),
            })?;

        loop {
            match self.writer_receiver.try_recv() {
                Ok(Ok(())) => return Ok(None),
                Ok(Err(error)) => return Err(ProcessError::Io(error)),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(ProcessError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "child stdin writer stopped without a result",
                    )));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if let Some(outcome) = self.pump(heartbeat)? {
                return Ok(Some(outcome));
            }
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin_sender.take();
    }

    pub fn receive_jsonl(
        &mut self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<JsonlReceive, ProcessError> {
        loop {
            if !self.stdout_lines.is_empty() {
                if let Some(outcome) = self.control_before_buffered_line(heartbeat)? {
                    return Ok(JsonlReceive::Terminal(outcome));
                }
                let line = self
                    .stdout_lines
                    .pop_front()
                    .expect("checked buffered line is present");
                return String::from_utf8(line)
                    .map(JsonlReceive::Line)
                    .map_err(|error| {
                        ProcessError::Io(io::Error::new(io::ErrorKind::InvalidData, error))
                    });
            }
            if let Some(outcome) = self.pump(heartbeat)? {
                return Ok(JsonlReceive::Terminal(outcome));
            }
            if self.stdout_closed {
                return Ok(JsonlReceive::Eof);
            }
        }
    }

    fn control_before_buffered_line(
        &mut self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Option<ProcessOutcome>, ProcessError> {
        if let Some(reason) = self.terminal.clone() {
            return self.interrupt(reason).map(Some);
        }
        // If completion is already observable, preserve its buffered protocol
        // output even when completion coincides with cancellation or deadline.
        if self.try_wait()?.is_some() {
            return Ok(None);
        }
        if self.cancellation.is_cancelled() {
            return self.interrupt(Interruption::Cancellation).map(Some);
        }
        let now = Instant::now();
        if now >= self.deadline {
            return self.interrupt(Interruption::Timeout).map(Some);
        }
        if now >= self.next_heartbeat {
            if let Err(message) = heartbeat.heartbeat() {
                return self
                    .interrupt(Interruption::HeartbeatFailure { message })
                    .map(Some);
            }
            self.next_heartbeat = Instant::now()
                .checked_add(self.heartbeat_interval)
                .expect("heartbeat interval was validated");
        }
        Ok(None)
    }

    pub fn wait(
        &mut self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<ProcessOutcome, ProcessError> {
        loop {
            if let Some(outcome) = self.pump(heartbeat)? {
                return Ok(outcome);
            }
        }
    }

    /// Stops a session after an adapter-level protocol failure and returns evidence.
    pub fn abort(&mut self) -> Result<ProcessEvidence, ProcessError> {
        self.terminate_group()?;
        self.drain_to_close()?;
        self.finish_readers()?;
        Ok(self.evidence())
    }

    fn pump(
        &mut self,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Option<ProcessOutcome>, ProcessError> {
        self.drain_ready()?;

        if let Some(reason) = self.terminal.clone() {
            return self.interrupt(reason).map(Some);
        }

        // Observable completion wins a simultaneous deadline/cancellation race.
        if let Some(status) = self.try_wait()? {
            self.close_stdin();
            self.drain_to_close()?;
            if let Some(reason) = self.terminal.clone() {
                return self.interrupt(reason).map(Some);
            }
            // Completion of the direct child is not proof that its process
            // group is empty: a descendant may have closed the captured pipes
            // and kept running. Always terminate the original group before
            // reporting completion. Callers executing untrusted programs must
            // additionally use a PID namespace because setsid(2) can escape a
            // process group.
            let pgid = self
                .child
                .as_ref()
                .expect("completed child is present")
                .id() as i32;
            signal_group(pgid, libc::SIGKILL)?;
            self.drain_to_close()?;
            if let Some(reason) = self.terminal.clone() {
                return self.interrupt(reason).map(Some);
            }
            self.finish_readers()?;
            return Ok(Some(ProcessOutcome::Completed {
                status,
                evidence: self.evidence(),
            }));
        }

        if self.cancellation.is_cancelled() {
            return self.interrupt(Interruption::Cancellation).map(Some);
        }
        let now = Instant::now();
        if now >= self.deadline {
            return self.interrupt(Interruption::Timeout).map(Some);
        }
        if now >= self.next_heartbeat {
            if let Err(message) = heartbeat.heartbeat() {
                return self
                    .interrupt(Interruption::HeartbeatFailure { message })
                    .map(Some);
            }
            self.next_heartbeat = Instant::now()
                .checked_add(self.heartbeat_interval)
                .expect("heartbeat interval was validated");
            return Ok(None);
        }

        let wait = self
            .limits
            .poll_interval
            .min(self.deadline.saturating_duration_since(now))
            .min(self.next_heartbeat.saturating_duration_since(now));
        match self.receiver.recv_timeout(wait) {
            Ok(event) => self.accept(event)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                self.stdout_closed = true;
                self.stderr_closed = true;
            }
        }
        Ok(None)
    }

    fn accept(&mut self, event: ReaderEvent) -> Result<(), ProcessError> {
        match event {
            ReaderEvent::Chunk(Stream::Stdout, bytes) => {
                self.stdout.push(&bytes);
                for byte in bytes {
                    self.stdout_line.push(byte);
                    if self.stdout_line.len() > self.limits.jsonl_line_bytes {
                        self.terminal.get_or_insert(Interruption::OutputLimit {
                            stream: "stdout JSONL line",
                            limit: self.limits.jsonl_line_bytes,
                        });
                    }
                    if byte == b'\n' {
                        let line = std::mem::take(&mut self.stdout_line);
                        self.stdout_lines.push_back(line);
                    }
                }
                if self.stdout.total > self.limits.stdout_bytes as u64 {
                    self.terminal.get_or_insert(Interruption::OutputLimit {
                        stream: "stdout",
                        limit: self.limits.stdout_bytes,
                    });
                }
            }
            ReaderEvent::Chunk(Stream::Stderr, bytes) => {
                self.stderr.push(&bytes);
                if self.stderr.total > self.limits.stderr_bytes as u64 {
                    self.terminal.get_or_insert(Interruption::OutputLimit {
                        stream: "stderr",
                        limit: self.limits.stderr_bytes,
                    });
                }
            }
            ReaderEvent::Closed(Stream::Stdout) => {
                self.stdout_closed = true;
                if !self.stdout_line.is_empty() {
                    self.stdout_lines
                        .push_back(std::mem::take(&mut self.stdout_line));
                }
            }
            ReaderEvent::Closed(Stream::Stderr) => self.stderr_closed = true,
            ReaderEvent::Error(error) => return Err(ProcessError::Io(error)),
        }
        Ok(())
    }

    fn drain_ready(&mut self) -> Result<(), ProcessError> {
        // A cap prevents a noisy child from starving heartbeat/deadline checks.
        for _ in 0..64 {
            match self.receiver.try_recv() {
                Ok(event) => {
                    self.accept(event)?;
                    if self.terminal.is_some() {
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }
        Ok(())
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        self.child
            .as_mut()
            .expect("live child is present")
            .try_wait()
            .map_err(ProcessError::Io)
    }

    fn interrupt(&mut self, cause: Interruption) -> Result<ProcessOutcome, ProcessError> {
        self.terminate_group()?;
        self.drain_to_close()?;
        self.finish_readers()?;
        let evidence = self.evidence();
        if self.risk == EffectRisk::AmbiguousOnInterruption {
            return Ok(ProcessOutcome::AmbiguousExternalState { cause, evidence });
        }
        Ok(match cause {
            Interruption::Timeout => ProcessOutcome::TimedOut(evidence),
            Interruption::Cancellation => ProcessOutcome::Cancelled(evidence),
            Interruption::OutputLimit { stream, limit } => ProcessOutcome::OutputLimit {
                stream,
                limit,
                evidence,
            },
            Interruption::HeartbeatFailure { message } => {
                ProcessOutcome::HeartbeatFailure { message, evidence }
            }
        })
    }

    fn terminate_group(&mut self) -> Result<(), ProcessError> {
        self.close_stdin();
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let pgid = child.id() as i32;
        signal_group(pgid, libc::SIGTERM)?;
        let end = Instant::now()
            .checked_add(self.limits.termination_grace)
            .expect("termination grace was validated");
        while group_exists(pgid)? && Instant::now() < end {
            let _ = child.try_wait()?;
            thread::sleep(Duration::from_millis(5));
        }
        if group_exists(pgid)? {
            signal_group(pgid, libc::SIGKILL)?;
        }
        let _ = child.wait()?;
        Ok(())
    }

    fn drain_to_close(&mut self) -> Result<(), ProcessError> {
        let end = Instant::now()
            .checked_add(self.limits.termination_grace)
            .expect("termination grace was validated");
        while !(self.stdout_closed && self.stderr_closed) && Instant::now() < end {
            match self.receiver.recv_timeout(Duration::from_millis(5)) {
                Ok(event) => self.accept(event)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    }

    fn finish_readers(&mut self) -> Result<(), ProcessError> {
        for handle in [
            &mut self.stdin_writer,
            &mut self.stdout_reader,
            &mut self.stderr_reader,
        ] {
            if let Some(handle) = handle.take() {
                handle.join().map_err(|_| ProcessError::IoWorkerPanicked)?;
            }
        }
        Ok(())
    }

    fn evidence(&self) -> ProcessEvidence {
        ProcessEvidence {
            stdout: self.stdout.evidence(),
            stderr: self.stderr.evidence(),
        }
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if self
            .child
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
        {
            let _ = self.terminate_group();
        }
        let _ = self.finish_readers();
    }
}

fn signal_group(pgid: i32, signal: i32) -> io::Result<()> {
    // Negative pid addresses the process group created at spawn.
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn group_exists(pgid: i32) -> io::Result<bool> {
    let result = unsafe { libc::kill(-pgid, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

#[derive(Clone, Copy, Debug)]
enum Stream {
    Stdout,
    Stderr,
}

enum ReaderEvent {
    Chunk(Stream, Vec<u8>),
    Closed(Stream),
    Error(io::Error),
}

fn spawn_reader(
    mut input: impl Read + Send + 'static,
    stream: Stream,
    sender: SyncSender<ReaderEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = vec![0; READ_CHUNK_BYTES];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(ReaderEvent::Closed(stream));
                    return;
                }
                Ok(count) => {
                    if sender
                        .send(ReaderEvent::Chunk(stream, buffer[..count].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(ReaderEvent::Error(error));
                    return;
                }
            }
        }
    })
}

fn spawn_writer(
    mut stdin: ChildStdin,
    receiver: Receiver<Vec<u8>>,
    sender: SyncSender<io::Result<()>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(message) = receiver.recv() {
            let result = stdin.write_all(&message).and_then(|()| stdin.flush());
            let failed = result.is_err();
            if sender.send(result).is_err() || failed {
                return;
            }
        }
    })
}

struct BoundedMessage {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedMessage {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedMessage {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("stdin JSONL message exceeds its bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Capture {
    limit: usize,
    total: u64,
    digest: Sha256,
    tail: VecDeque<u8>,
}

impl Capture {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            total: 0,
            digest: Sha256::new(),
            tail: VecDeque::with_capacity(limit),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        self.digest.update(bytes);
        for byte in bytes {
            if self.tail.len() == self.limit {
                self.tail.pop_front();
            }
            self.tail.push_back(*byte);
        }
    }

    fn evidence(&self) -> OutputEvidence {
        let retained = self.tail.iter().copied().collect::<Vec<_>>();
        OutputEvidence {
            total_bytes: self.total,
            retained_bytes: retained.len(),
            sha256: format!("{:x}", self.digest.clone().finalize()),
            truncated: self.total > retained.len() as u64,
            tail: String::from_utf8_lossy(&retained).into_owned(),
            tail_bytes: retained,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path, process::Command, thread};

    fn limits() -> ProcessLimits {
        ProcessLimits {
            stdin_message_bytes: 4 * 1024,
            stdout_bytes: 4 * 1024,
            stderr_bytes: 4 * 1024,
            jsonl_line_bytes: 1024,
            final_message_bytes: 1024,
            termination_grace: Duration::from_millis(40),
            poll_interval: Duration::from_millis(2),
        }
    }

    fn supervisor(token: CancellationToken) -> ProcessSupervisor {
        ProcessSupervisor::new(Duration::from_millis(5), limits(), token).unwrap()
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[test]
    fn never_exiting_child_reaches_absolute_deadline() {
        let mut command = shell("while :; do :; done");
        let outcome = supervisor(CancellationToken::new())
            .run(
                &mut command,
                Instant::now() + Duration::from_millis(30),
                EffectRisk::None,
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert!(matches!(outcome, ProcessOutcome::TimedOut(_)));
    }

    #[test]
    fn cancellation_kills_descendants_in_the_child_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let script = format!(
            "(trap '' TERM; while :; do :; done) & echo $! > '{}'; wait",
            shell_quote(&pid_path)
        );
        let token = CancellationToken::new();
        let mut command = shell(&script);
        let mut child = supervisor(token.clone())
            .spawn(
                &mut command,
                Instant::now() + Duration::from_secs(5),
                EffectRisk::None,
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() && Instant::now() < end {
            thread::sleep(Duration::from_millis(2));
        }
        let descendant: i32 = fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        token.cancel();
        let outcome = child.wait(&mut NoopHeartbeat).unwrap();
        assert!(matches!(outcome, ProcessOutcome::Cancelled(_)));
        let state = fs::read_to_string(format!("/proc/{descendant}/stat"))
            .ok()
            .and_then(|stat| stat.split_whitespace().nth(2).map(str::to_owned));
        // Linux can expose a killed process briefly as either zombie (`Z`) or
        // dead (`X`) before removing its procfs entry. Neither state executes.
        assert!(
            state.is_none() || matches!(state.as_deref(), Some("Z" | "X")),
            "descendant {descendant} survived group cancellation in state {state:?}"
        );
    }

    #[test]
    fn normal_completion_kills_silent_descendants_in_the_original_group() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let script = format!(
            "sleep 30 </dev/null >/dev/null 2>&1 & echo $! > '{}'",
            shell_quote(&pid_path)
        );
        let mut command = shell(&script);
        let outcome = supervisor(CancellationToken::new())
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::Completed { status, .. } if status.success()
        ));
        let descendant: i32 = fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(1);
        let state = loop {
            let state = fs::read_to_string(format!("/proc/{descendant}/stat"))
                .ok()
                .and_then(|stat| stat.split_whitespace().nth(2).map(str::to_owned));
            if state.is_none() || matches!(state.as_deref(), Some("Z" | "X")) {
                break state;
            }
            if Instant::now() >= end {
                break state;
            }
            thread::sleep(Duration::from_millis(2));
        };
        assert!(
            state.is_none() || matches!(state.as_deref(), Some("Z" | "X")),
            "silent descendant {descendant} survived normal completion in state {state:?}"
        );
    }

    #[test]
    fn continuous_output_is_stopped_with_tail_digest_and_truncation_evidence() {
        let mut configured = limits();
        configured.stdout_bytes = 1024;
        configured.jsonl_line_bytes = 1024;
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(5),
            configured,
            CancellationToken::new(),
        )
        .unwrap();
        let mut command = shell("yes 0123456789");
        let outcome = supervisor
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(2),
                EffectRisk::None,
                &mut NoopHeartbeat,
            )
            .unwrap();
        let ProcessOutcome::OutputLimit {
            stream, evidence, ..
        } = outcome
        else {
            panic!("expected output limit");
        };
        assert_eq!(stream, "stdout");
        assert!(evidence.stdout.total_bytes > 1024);
        assert_eq!(evidence.stdout.retained_bytes, 1024);
        assert!(evidence.stdout.truncated);
        assert_eq!(evidence.stdout.sha256.len(), 64);
    }

    #[test]
    fn completed_child_cannot_bypass_the_output_limit() {
        let mut configured = limits();
        configured.stdout_bytes = 1024;
        configured.jsonl_line_bytes = 2048;
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(5),
            configured,
            CancellationToken::new(),
        )
        .unwrap();
        let mut command = shell("head -c 2048 /dev/zero");
        let outcome = supervisor
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::OutputLimit {
                stream: "stdout",
                limit: 1024,
                ..
            }
        ));
    }

    #[test]
    fn oversized_jsonl_line_is_stopped_before_unbounded_accumulation() {
        let mut configured = limits();
        configured.stdout_bytes = 16 * 1024;
        configured.jsonl_line_bytes = 128;
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(5),
            configured,
            CancellationToken::new(),
        )
        .unwrap();
        let mut command = shell("while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done");
        let mut child = supervisor
            .spawn(
                &mut command,
                Instant::now() + Duration::from_secs(2),
                EffectRisk::None,
            )
            .unwrap();
        let received = child.receive_jsonl(&mut NoopHeartbeat).unwrap();
        assert!(matches!(
            received,
            JsonlReceive::Terminal(ProcessOutcome::OutputLimit {
                stream: "stdout JSONL line",
                limit: 128,
                ..
            })
        ));
    }

    #[test]
    fn oversized_stdin_message_is_rejected_with_a_typed_limit_outcome() {
        let mut configured = limits();
        configured.stdin_message_bytes = 128;
        let supervisor = ProcessSupervisor::new(
            Duration::from_millis(5),
            configured,
            CancellationToken::new(),
        )
        .unwrap();
        let mut command = shell("while :; do :; done");
        let mut child = supervisor
            .spawn(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
            )
            .unwrap();

        let outcome = child
            .write_json(
                &serde_json::json!({"payload": "x".repeat(1024)}),
                &mut NoopHeartbeat,
            )
            .unwrap()
            .expect("oversized input terminates the child");

        assert!(matches!(
            outcome,
            ProcessOutcome::OutputLimit {
                stream: "stdin JSONL message",
                limit: 128,
                ..
            }
        ));
    }

    #[test]
    fn completed_child_wins_a_deadline_race_when_exit_is_already_observable() {
        let mut command = shell("exit 0");
        let mut child = supervisor(CancellationToken::new())
            .spawn(&mut command, Instant::now(), EffectRisk::None)
            .unwrap();
        thread::sleep(Duration::from_millis(20));
        let outcome = child.wait(&mut NoopHeartbeat).unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::Completed { status, .. } if status.success()
        ));
    }

    #[test]
    fn completed_child_wins_a_cancellation_race_when_exit_is_already_observable() {
        let cancellation = CancellationToken::new();
        let mut command = shell("exit 0");
        let mut child = supervisor(cancellation.clone())
            .spawn(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
            )
            .unwrap();
        thread::sleep(Duration::from_millis(20));
        cancellation.cancel();

        let outcome = child.wait(&mut NoopHeartbeat).unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::Completed { status, .. } if status.success()
        ));
    }

    #[test]
    fn heartbeat_failure_stops_the_child_with_a_typed_outcome() {
        struct FailingHeartbeat;
        impl ProcessHeartbeat for FailingHeartbeat {
            fn heartbeat(&mut self) -> Result<(), String> {
                Err("store is unavailable".to_owned())
            }
        }

        let mut command = shell("while :; do :; done");
        let outcome = supervisor(CancellationToken::new())
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
                &mut FailingHeartbeat,
            )
            .unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::HeartbeatFailure { ref message, .. }
                if message == "store is unavailable"
        ));
    }

    #[test]
    fn heartbeats_continue_while_the_child_emits_output() {
        struct CountingHeartbeat(usize);
        impl ProcessHeartbeat for CountingHeartbeat {
            fn heartbeat(&mut self) -> Result<(), String> {
                self.0 += 1;
                Ok(())
            }
        }

        let mut heartbeat = CountingHeartbeat(0);
        let mut command =
            shell("i=0; while [ $i -lt 20 ]; do echo progress; i=$((i + 1)); sleep 0.002; done");
        let outcome = supervisor(CancellationToken::new())
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
                &mut heartbeat,
            )
            .unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::Completed { status, .. } if status.success()
        ));
        assert!(heartbeat.0 > 0);
    }

    #[test]
    fn interrupted_effect_is_reported_as_ambiguous_external_state() {
        let token = CancellationToken::new();
        token.cancel();
        let mut command = shell("while :; do :; done");
        let outcome = supervisor(token)
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::AmbiguousOnInterruption,
                &mut NoopHeartbeat,
            )
            .unwrap();
        assert!(matches!(
            outcome,
            ProcessOutcome::AmbiguousExternalState {
                cause: Interruption::Cancellation,
                ..
            }
        ));
    }

    #[test]
    fn completed_output_retains_exact_tail_and_digest_evidence() {
        let mut command = shell("printf 'abc\\n'; printf 'problem\\n' >&2");
        let outcome = supervisor(CancellationToken::new())
            .run(
                &mut command,
                Instant::now() + Duration::from_secs(1),
                EffectRisk::None,
                &mut NoopHeartbeat,
            )
            .unwrap();
        let ProcessOutcome::Completed { evidence, .. } = outcome else {
            panic!("expected completion");
        };
        assert_eq!(evidence.stdout.tail, "abc\n");
        assert_eq!(
            evidence.stdout.sha256,
            format!("{:x}", Sha256::digest(b"abc\n"))
        );
        assert_eq!(evidence.stderr.tail, "problem\n");
        assert!(!evidence.stdout.truncated);
    }

    fn shell_quote(path: &Path) -> String {
        path.to_string_lossy().replace('\'', "'\\''")
    }
}
