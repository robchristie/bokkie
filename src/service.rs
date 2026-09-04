//! Failure-isolated background execution lanes and deterministic service runner.

use std::{
    fmt, io,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::gardener_runner::{GardenerRunner, GardenerRunnerError, GardenerRuntimeConfig};
use crate::process::CancellationToken;
use crate::{
    Claim, Completion, ExecutionLane, FailureDisposition, RunResult, Runner, Store, StoreError,
    SystemClock, UnixClock,
};

/// Upper bound for ordinary fake-runner capacity in one service process.
pub const MAX_ORDINARY_CONCURRENCY: usize = 32;
/// Default ordinary capacity. Gardener capacity is always exactly one when enabled.
pub const DEFAULT_ORDINARY_CONCURRENCY: usize = 4;
/// Maximum time allowed for all lane workers to reconcile after cancellation.
pub const LANE_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct ClaimAdmission {
    inner: Arc<ClaimAdmissionInner>,
}

#[derive(Debug)]
struct ClaimAdmissionInner {
    closed: Arc<AtomicBool>,
    state: Mutex<AdmissionState>,
    ready: Condvar,
}

#[derive(Debug)]
struct AdmissionState {
    active: bool,
    waiting: [usize; 2],
    next_lane: ExecutionLane,
}

impl ClaimAdmission {
    fn new() -> Self {
        Self {
            inner: Arc::new(ClaimAdmissionInner {
                closed: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(AdmissionState {
                    active: false,
                    waiting: [0, 0],
                    next_lane: ExecutionLane::Gardener,
                }),
                ready: Condvar::new(),
            }),
        }
    }

    /// Atomically stop claim admission and publish cancellation to active work.
    pub fn close(&self) {
        // Publish closure before waiting for an already-admitted claim. Any
        // later mutex winner observes `closed` and cannot enter its Store call.
        self.inner.closed.store(true, Ordering::SeqCst);
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.inner.ready.notify_all();
        while state.active {
            state = self
                .inner
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    fn cancellation_token(&self) -> CancellationToken {
        CancellationToken::from_flag(Arc::clone(&self.inner.closed))
    }

    /// Run one Store claim only if admission remains open and this lane owns
    /// the next fair turn. Claim errors and panics close admission before the
    /// next waiter can enter Store.
    fn claim<T, E>(
        &self,
        lane: ExecutionLane,
        claim: impl FnOnce() -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        let lane_index = lane_index(lane);
        let other_index = 1 - lane_index;
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.waiting[lane_index] += 1;
        loop {
            if self.is_closed() {
                state.waiting[lane_index] -= 1;
                self.inner.ready.notify_all();
                return Ok(None);
            }
            let other_waiting = state.waiting[other_index] > 0;
            if !state.active && (!other_waiting || state.next_lane == lane) {
                break;
            }
            state = self
                .inner
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.waiting[lane_index] -= 1;
        state.active = true;
        state.next_lane = other_lane(lane);
        drop(state);

        let result = panic::catch_unwind(AssertUnwindSafe(claim));
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active = false;
        match &result {
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => self.inner.closed.store(true, Ordering::SeqCst),
        }
        self.inner.ready.notify_all();
        drop(state);

        match result {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(error)) => Err(error),
            Err(payload) => panic::resume_unwind(payload),
        }
    }
}

fn lane_index(lane: ExecutionLane) -> usize {
    match lane {
        ExecutionLane::Ordinary => 0,
        ExecutionLane::Gardener => 1,
        ExecutionLane::Outbox => panic!("outbox claim admission is not implemented"),
    }
}

fn other_lane(lane: ExecutionLane) -> ExecutionLane {
    match lane {
        ExecutionLane::Ordinary => ExecutionLane::Gardener,
        ExecutionLane::Gardener => ExecutionLane::Ordinary,
        ExecutionLane::Outbox => panic!("outbox claim admission is not implemented"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceFakeOutcome {
    Succeed,
    FailRetryable,
    FailTerminal,
}

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub database: PathBuf,
    pub poll_interval: Duration,
    pub lease_seconds: i64,
    pub ordinary_concurrency: usize,
    pub fake_delay: Duration,
    pub fake_outcome: ServiceFakeOutcome,
}

impl SchedulerConfig {
    fn validate(&self) -> Result<(), SchedulerError> {
        if !(1..=MAX_ORDINARY_CONCURRENCY).contains(&self.ordinary_concurrency) {
            return Err(SchedulerError::Configuration(format!(
                "ordinary concurrency must be between 1 and {MAX_ORDINARY_CONCURRENCY}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LaneFailureCause {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Gardener(#[from] GardenerRunnerError),
    #[error("worker thread panicked")]
    Panicked,
    #[error("worker stopped without a shutdown request")]
    StoppedUnexpectedly,
    #[error("could not start worker thread: {0}")]
    Thread(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("invalid scheduler configuration: {0}")]
    Configuration(String),
    #[error("{0}")]
    Shutdown(#[source] Box<LaneShutdownFailure>),
    #[error("could not start scheduler supervisor thread: {0}")]
    Thread(#[source] io::Error),
}

impl SchedulerError {
    pub fn is_configuration(&self) -> bool {
        match self {
            Self::Configuration(_) => true,
            Self::Shutdown(failure) => failure.primary.as_ref().is_some_and(|primary| {
                matches!(
                    primary.cause.as_ref(),
                    LaneFailureCause::Gardener(GardenerRunnerError::Configuration(_))
                )
            }),
            Self::Thread(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionWorkerId {
    pub lane: ExecutionLane,
    /// One-based worker number within the lane.
    pub slot: usize,
}

impl fmt::Display for ExecutionWorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}[{}]", self.lane, self.slot)
    }
}

#[derive(Debug)]
pub struct LaneFailure {
    pub lane: ExecutionLane,
    pub cause: Box<LaneFailureCause>,
}

#[derive(Debug)]
pub struct LaneShutdownFailure {
    pub primary: Option<LaneFailure>,
    pub timed_out: Vec<ExecutionWorkerId>,
}

impl fmt::Display for LaneShutdownFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("execution lane shutdown failed")?;
        if let Some(primary) = &self.primary {
            write!(
                formatter,
                "; primary {} failure: {}",
                primary.lane, primary.cause
            )?;
        }
        if !self.timed_out.is_empty() {
            formatter.write_str("; timed out workers: ")?;
            for (index, worker) in self.timed_out.iter().enumerate() {
                if index > 0 {
                    formatter.write_str(", ")?;
                }
                write!(formatter, "{worker}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for LaneShutdownFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.primary
            .as_ref()
            .map(|primary| primary.cause.as_ref() as &(dyn std::error::Error + 'static))
    }
}

struct WorkerExit {
    id: ExecutionWorkerId,
    result: Result<(), LaneFailureCause>,
}

struct Worker {
    id: ExecutionWorkerId,
    thread: thread::JoinHandle<()>,
}

pub struct Scheduler {
    admission: ClaimAdmission,
    thread: Option<thread::JoinHandle<Result<(), SchedulerError>>>,
    exit: Option<oneshot::Receiver<()>>,
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Dropping the owner must never detach workers with admission still
        // enabled. The supervisor remains responsible for bounded joining when
        // `shutdown` is used; this is the best-effort lease-safe fallback.
        self.admission.close();
    }
}

impl Scheduler {
    pub fn start(config: SchedulerConfig) -> Result<Self, SchedulerError> {
        Self::start_configured(config, None)
    }

    /// Starts independently supervised ordinary and optional gardener lanes.
    /// Gardener-bound obligations are not claimable when `gardener` is `None`.
    pub fn start_configured(
        config: SchedulerConfig,
        gardener: Option<GardenerRuntimeConfig>,
    ) -> Result<Self, SchedulerError> {
        config.validate()?;
        // Startup owns migration. The scheduler and every worker are only
        // compatible-schema consumers.
        Store::open_compatible(&config.database).map_err(|cause| {
            shutdown_error(Some((ExecutionLane::Ordinary, cause.into())), Vec::new())
        })?;
        if let Some(runtime) = &gardener {
            runtime.validate(config.lease_seconds).map_err(|cause| {
                shutdown_error(Some((ExecutionLane::Gardener, cause.into())), Vec::new())
            })?;
        }

        let admission = ClaimAdmission::new();
        let gardener =
            gardener.map(|runtime| runtime.with_cancellation(admission.cancellation_token()));
        let supervisor_admission = admission.clone();
        let (exit_sender, exit) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("bokkie-scheduler-supervisor".to_owned())
            .spawn(move || scheduler_loop(config, gardener, supervisor_admission, exit_sender))
            .map_err(SchedulerError::Thread)?;
        Ok(Self {
            admission,
            thread: Some(thread),
            exit: Some(exit),
        })
    }

    pub fn admission(&self) -> ClaimAdmission {
        self.admission.clone()
    }

    /// Resolve as soon as a lane failure requests service shutdown, or when all
    /// workers have otherwise returned. Lane reconciliation may still be in progress.
    pub fn take_exit_signal(&mut self) -> oneshot::Receiver<()> {
        self.exit
            .take()
            .expect("scheduler exit signal can only be taken once")
    }

    /// Stop admitting claims, cancel active children, and wait for bounded reconciliation.
    pub fn shutdown(mut self) -> Result<(), SchedulerError> {
        self.admission.close();
        self.thread
            .take()
            .expect("scheduler supervisor thread is present")
            .join()
            .map_err(|_| {
                shutdown_error(
                    Some((ExecutionLane::Ordinary, LaneFailureCause::Panicked)),
                    Vec::new(),
                )
            })?
    }
}

fn scheduler_loop(
    config: SchedulerConfig,
    gardener: Option<GardenerRuntimeConfig>,
    admission: ClaimAdmission,
    exit_sender: oneshot::Sender<()>,
) -> Result<(), SchedulerError> {
    let (sender, receiver) = mpsc::channel();
    let capacity = config.ordinary_concurrency + usize::from(gardener.is_some());
    let mut workers = Vec::with_capacity(capacity);
    let mut first_failure = None;

    for slot in 0..config.ordinary_concurrency {
        let worker_config = config.clone();
        let worker_admission = admission.clone();
        let id = ExecutionWorkerId {
            lane: ExecutionLane::Ordinary,
            slot: slot + 1,
        };
        match spawn_worker(id, sender.clone(), admission.clone(), move || {
            ordinary_lane_loop(&worker_config, &worker_admission)
        }) {
            Ok(worker) => workers.push(worker),
            Err(cause) => {
                first_failure = Some((id.lane, cause));
                admission.close();
                break;
            }
        }
    }

    if first_failure.is_none() {
        if let Some(runtime) = gardener {
            let worker_config = config.clone();
            let worker_admission = admission.clone();
            let id = ExecutionWorkerId {
                lane: ExecutionLane::Gardener,
                slot: 1,
            };
            match spawn_worker(id, sender.clone(), admission.clone(), move || {
                gardener_lane_loop(&worker_config, &runtime, &worker_admission)
            }) {
                Ok(worker) => workers.push(worker),
                Err(cause) => {
                    first_failure = Some((id.lane, cause));
                    admission.close();
                }
            }
        }
    }
    drop(sender);

    supervise_workers(
        workers,
        receiver,
        admission,
        exit_sender,
        first_failure,
        LANE_JOIN_TIMEOUT,
    )
}

fn supervise_workers(
    mut workers: Vec<Worker>,
    receiver: mpsc::Receiver<WorkerExit>,
    admission: ClaimAdmission,
    exit_sender: oneshot::Sender<()>,
    mut first_failure: Option<(ExecutionLane, LaneFailureCause)>,
    join_timeout: Duration,
) -> Result<(), SchedulerError> {
    let mut exit_sender = Some(exit_sender);
    if first_failure.is_some() {
        let _ = exit_sender.take().expect("exit sender is present").send(());
    }
    let mut stop_deadline = admission.is_closed().then(|| Instant::now() + join_timeout);

    while !workers.is_empty() {
        if admission.is_closed() && stop_deadline.is_none() {
            stop_deadline = Some(Instant::now() + join_timeout);
        }
        let wait = stop_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(100));
        if wait.is_zero() {
            return terminal_result(first_failure, timed_out_workers(&workers));
        }

        match receiver.recv_timeout(wait) {
            Ok(worker_exit) => {
                let position = workers
                    .iter()
                    .position(|worker| worker.id == worker_exit.id)
                    .expect("every worker reports exactly once");
                let worker = workers.swap_remove(position);
                // The completion message is sent at the end of the closure, so this
                // join cannot wait on lane work.
                let _ = worker.thread.join();
                let unexpected_success = worker_exit.result.is_ok() && !admission.is_closed();
                if first_failure.is_none() && (worker_exit.result.is_err() || unexpected_success) {
                    let cause = worker_exit
                        .result
                        .err()
                        .unwrap_or(LaneFailureCause::StoppedUnexpectedly);
                    first_failure = Some((worker_exit.id.lane, cause));
                    admission.close();
                    stop_deadline = Some(Instant::now() + join_timeout);
                    let _ = exit_sender.take().expect("exit sender is present").send(());
                }
            }
            Err(RecvTimeoutError::Timeout) if stop_deadline.is_none() => {}
            Err(RecvTimeoutError::Timeout) => {
                return terminal_result(first_failure, timed_out_workers(&workers));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let lane = workers[0].id.lane;
                return Err(shutdown_error(
                    Some((lane, LaneFailureCause::Panicked)),
                    timed_out_workers(&workers),
                ));
            }
        }
    }

    if let Some(sender) = exit_sender.take() {
        let _ = sender.send(());
    }
    terminal_result(first_failure, Vec::new())
}

fn timed_out_workers(workers: &[Worker]) -> Vec<ExecutionWorkerId> {
    let mut timed_out: Vec<_> = workers.iter().map(|worker| worker.id).collect();
    timed_out.sort_by_key(|worker| {
        let lane = match worker.lane {
            ExecutionLane::Ordinary => 0,
            ExecutionLane::Gardener => 1,
            ExecutionLane::Outbox => 2,
        };
        (lane, worker.slot)
    });
    timed_out
}

fn terminal_result(
    failure: Option<(ExecutionLane, LaneFailureCause)>,
    timed_out: Vec<ExecutionWorkerId>,
) -> Result<(), SchedulerError> {
    if failure.is_some() || !timed_out.is_empty() {
        Err(shutdown_error(failure, timed_out))
    } else {
        Ok(())
    }
}

fn shutdown_error(
    failure: Option<(ExecutionLane, LaneFailureCause)>,
    timed_out: Vec<ExecutionWorkerId>,
) -> SchedulerError {
    SchedulerError::Shutdown(Box::new(LaneShutdownFailure {
        primary: failure.map(|(lane, cause)| LaneFailure {
            lane,
            cause: Box::new(cause),
        }),
        timed_out,
    }))
}

fn spawn_worker(
    id: ExecutionWorkerId,
    sender: mpsc::Sender<WorkerExit>,
    admission: ClaimAdmission,
    work: impl FnOnce() -> Result<(), LaneFailureCause> + Send + 'static,
) -> Result<Worker, LaneFailureCause> {
    let name = format!("bokkie-{}-{}", id.lane, id.slot);
    let thread = thread::Builder::new()
        .name(name)
        .spawn(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(work))
                .map_err(|_| LaneFailureCause::Panicked)
                .and_then(|result| result);
            if result.is_err() {
                admission.close();
            }
            let _ = sender.send(WorkerExit { id, result });
        })
        .map_err(LaneFailureCause::Thread)?;
    Ok(Worker { id, thread })
}

fn ordinary_lane_loop(
    config: &SchedulerConfig,
    admission: &ClaimAdmission,
) -> Result<(), LaneFailureCause> {
    let mut store = close_on_error(admission, Store::open_compatible(&config.database))?;
    let clock = SystemClock;

    while !admission.is_closed() {
        let Some(mut claims) = admission.claim(ExecutionLane::Ordinary, || {
            store.claim_due(clock.now(), config.lease_seconds, 1)
        })?
        else {
            break;
        };
        if let Some(claim) = claims.pop() {
            let cancelled = close_on_error(
                admission,
                delay_with_lease_renewal(
                    &mut store,
                    &claim,
                    config.fake_delay,
                    config.lease_seconds,
                    &clock,
                    admission,
                ),
            )?;
            let result = if cancelled {
                cancelled_fake_result(&claim)
            } else {
                let mut runner = ServiceFakeRunner {
                    outcome: config.fake_outcome,
                };
                runner.execute(&claim)
            };
            close_on_error(
                admission,
                reconcile_completion(&mut store, &claim, result, &clock),
            )?;
            continue;
        }
        sleep_until_poll_or_stop(config.poll_interval, admission);
    }
    Ok(())
}

fn gardener_lane_loop(
    config: &SchedulerConfig,
    runtime: &GardenerRuntimeConfig,
    admission: &ClaimAdmission,
) -> Result<(), LaneFailureCause> {
    let mut store = close_on_error(admission, Store::open_compatible(&config.database))?;
    let clock = SystemClock;
    let runner = close_on_error(
        admission,
        GardenerRunner::new(runtime, config.lease_seconds, &clock),
    )?;

    while !admission.is_closed() {
        let Some(mut claims) = admission.claim(ExecutionLane::Gardener, || {
            store.claim_due_gardener(clock.now(), config.lease_seconds, 1)
        })?
        else {
            break;
        };
        if let Some(claim) = claims.pop() {
            let result = runner.execute(&mut store, &claim);
            close_on_error(
                admission,
                reconcile_completion(&mut store, &claim, result, &clock),
            )?;
            continue;
        }
        sleep_until_poll_or_stop(config.poll_interval, admission);
    }
    Ok(())
}

fn reconcile_completion(
    store: &mut Store,
    claim: &Claim,
    result: RunResult,
    clock: &impl UnixClock,
) -> Result<(), StoreError> {
    match store.complete(claim, result.completion, clock.now()) {
        Ok(()) | Err(StoreError::Fenced) => Ok(()),
        Err(error) => Err(error),
    }
}

fn close_on_error<T, E>(admission: &ClaimAdmission, result: Result<T, E>) -> Result<T, E> {
    if result.is_err() {
        admission.close();
    }
    result
}

fn sleep_until_poll_or_stop(interval: Duration, admission: &ClaimAdmission) {
    let quantum = Duration::from_millis(25);
    let mut remaining = interval;
    while !remaining.is_zero() && !admission.is_closed() {
        let sleep_for = remaining.min(quantum);
        thread::sleep(sleep_for);
        remaining = remaining.saturating_sub(sleep_for);
    }
}

fn delay_with_lease_renewal(
    store: &mut Store,
    claim: &Claim,
    delay: Duration,
    lease_seconds: i64,
    clock: &impl UnixClock,
    admission: &ClaimAdmission,
) -> Result<bool, StoreError> {
    let started = Instant::now();
    let mut lease_expires_at = claim.lease_expires_at;
    while started.elapsed() < delay {
        if admission.is_closed() {
            return Ok(true);
        }
        let now = clock.now();
        if now >= lease_expires_at.saturating_sub(1) {
            lease_expires_at = store.renew_lease(claim, now, lease_seconds)?;
        }
        let remaining = delay.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(25)));
    }
    Ok(admission.is_closed())
}

fn cancelled_fake_result(claim: &Claim) -> RunResult {
    RunResult {
        completion: Completion::Failed {
            disposition: FailureDisposition::Cancelled,
            error: "ordinary execution cancelled during service shutdown".to_owned(),
            evidence: Some(format!(
                "shutdown cancelled fake attempt {} before its configured delay elapsed",
                claim.attempt_number
            )),
        },
    }
}

struct ServiceFakeRunner {
    outcome: ServiceFakeOutcome,
}

impl Runner for ServiceFakeRunner {
    fn execute(&mut self, claim: &Claim) -> RunResult {
        let completion = match self.outcome {
            ServiceFakeOutcome::Succeed => Completion::Succeeded {
                evidence: Some(format!(
                    "deterministic fake success for attempt {}",
                    claim.attempt_number
                )),
            },
            ServiceFakeOutcome::FailRetryable => Completion::Failed {
                disposition: FailureDisposition::RetrySafe,
                error: "deterministic retryable fake failure".to_owned(),
                evidence: Some("fake runner configured to fail retryably".to_owned()),
            },
            ServiceFakeOutcome::FailTerminal => Completion::Failed {
                disposition: FailureDisposition::Terminal,
                error: "deterministic terminal fake failure".to_owned(),
                evidence: Some("fake runner configured to fail terminally".to_owned()),
            },
        };
        RunResult { completion }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Barrier, mpsc};

    use tempfile::TempDir;

    use super::*;
    use crate::{NewObligation, ObligationState, RetryPolicy};

    fn config(
        database: PathBuf,
        ordinary_concurrency: usize,
        fake_delay: Duration,
    ) -> SchedulerConfig {
        SchedulerConfig {
            database,
            poll_interval: Duration::from_millis(5),
            lease_seconds: 2,
            ordinary_concurrency,
            fake_delay,
            fake_outcome: ServiceFakeOutcome::Succeed,
        }
    }

    fn create_due(store: &mut Store, id: &str) {
        store
            .create(
                NewObligation {
                    id: id.to_owned(),
                    description: format!("work for {id}"),
                    scheduled_at: 1,
                    recurrence: None,
                    approval_required: false,
                    retry: RetryPolicy::default(),
                },
                1,
            )
            .unwrap();
    }

    fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        while !condition() {
            assert!(Instant::now() < deadline, "condition did not become true");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn seed_lane_backlogs(database: &std::path::Path, per_lane: usize) {
        let mut store = Store::open(database).unwrap();
        for index in 0..per_lane {
            create_due(&mut store, &format!("ordinary-fair-{index}"));
            let gardener_id = format!("gardener-fair-{index}");
            create_due(&mut store, &gardener_id);
            let connection = rusqlite::Connection::open(database).unwrap();
            connection
                .execute(
                    "INSERT INTO gardener_obligation_bindings(obligation_id, kind, created_at)
                     VALUES (?1, 'inspection', 1)",
                    [&gardener_id],
                )
                .unwrap();
        }
    }

    fn claim_store_lane(store: &mut Store, lane: ExecutionLane) -> Result<Vec<Claim>, StoreError> {
        match lane {
            ExecutionLane::Ordinary => store.claim_due(SystemClock.now(), 30, 1),
            ExecutionLane::Gardener => store.claim_due_gardener(SystemClock.now(), 30, 1),
            ExecutionLane::Outbox => unreachable!(),
        }
    }

    fn spawn_store_claimant(
        database: PathBuf,
        admission: ClaimAdmission,
        lane: ExecutionLane,
        turn_sender: mpsc::Sender<ExecutionLane>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut store = Store::open_compatible(database).unwrap();
            let claims = admission
                .claim(lane, || {
                    turn_sender.send(lane).unwrap();
                    claim_store_lane(&mut store, lane)
                })
                .unwrap()
                .expect("admission remains open");
            assert_eq!(claims.len(), 1, "{lane} backlog unexpectedly exhausted");
        })
    }

    fn assert_store_claims_alternate_after(leader_lane: ExecutionLane) {
        const ORDINARY_WAITERS: usize = 8;
        const GARDENER_WAITERS: usize = 3;
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("fair-admission.sqlite");
        seed_lane_backlogs(&database, 16);
        let admission = ClaimAdmission::new();
        let (turn_sender, turn_receiver) = mpsc::channel();
        let (leader_active_sender, leader_active_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        let leader_database = database.clone();
        let leader_admission = admission.clone();
        let leader_turn_sender = turn_sender.clone();
        let leader = thread::spawn(move || {
            let mut store = Store::open_compatible(leader_database).unwrap();
            let claims = leader_admission
                .claim(leader_lane, || {
                    leader_turn_sender.send(leader_lane).unwrap();
                    let claims = claim_store_lane(&mut store, leader_lane)?;
                    leader_active_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                    Ok::<_, StoreError>(claims)
                })
                .unwrap()
                .unwrap();
            assert_eq!(claims.len(), 1);
        });
        leader_active_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            turn_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            leader_lane
        );

        let mut claimants = Vec::new();
        for _ in 0..ORDINARY_WAITERS {
            claimants.push(spawn_store_claimant(
                database.clone(),
                admission.clone(),
                ExecutionLane::Ordinary,
                turn_sender.clone(),
            ));
        }
        for _ in 0..GARDENER_WAITERS {
            claimants.push(spawn_store_claimant(
                database.clone(),
                admission.clone(),
                ExecutionLane::Gardener,
                turn_sender.clone(),
            ));
        }
        wait_until(Duration::from_secs(2), || {
            admission
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .waiting
                == [ORDINARY_WAITERS, GARDENER_WAITERS]
        });
        release_sender.send(()).unwrap();

        let mut expected = other_lane(leader_lane);
        for _ in 0..(GARDENER_WAITERS * 2) {
            assert_eq!(
                turn_receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
                expected
            );
            expected = other_lane(expected);
        }
        leader.join().unwrap();
        for claimant in claimants {
            claimant.join().unwrap();
        }
    }

    #[test]
    fn ordinary_capacity_is_bounded_and_shutdown_admits_no_more_claims() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("capacity.sqlite");
        let mut store = Store::open(&database).unwrap();
        for index in 0..6 {
            create_due(&mut store, &format!("ordinary-{index}"));
        }
        drop(store);

        let scheduler =
            Scheduler::start(config(database.clone(), 2, Duration::from_secs(30))).unwrap();
        wait_until(Duration::from_secs(2), || {
            Store::open_compatible(&database)
                .unwrap()
                .list()
                .unwrap()
                .iter()
                .filter(|obligation| obligation.state == ObligationState::Running)
                .count()
                == 2
        });
        let admission = scheduler.admission();
        admission.close();
        scheduler.shutdown().unwrap();

        let obligations = Store::open_compatible(&database).unwrap().list().unwrap();
        assert_eq!(
            obligations
                .iter()
                .filter(|obligation| obligation.failure_disposition
                    == Some(FailureDisposition::Cancelled))
                .count(),
            2
        );
        assert_eq!(
            obligations
                .iter()
                .filter(|obligation| obligation.state == ObligationState::Pending)
                .count(),
            4,
            "workers must not claim queued work after the shared stop gate closes"
        );
    }

    #[test]
    fn dropping_scheduler_stops_admission_and_reconciles_claimed_work() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("drop.sqlite");
        let mut store = Store::open(&database).unwrap();
        for index in 0..3 {
            create_due(&mut store, &format!("drop-{index}"));
        }
        drop(store);

        let scheduler =
            Scheduler::start(config(database.clone(), 1, Duration::from_secs(30))).unwrap();
        wait_until(Duration::from_secs(2), || {
            Store::open_compatible(&database)
                .unwrap()
                .list()
                .unwrap()
                .iter()
                .any(|obligation| obligation.state == ObligationState::Running)
        });
        drop(scheduler);

        wait_until(Duration::from_secs(2), || {
            let obligations = Store::open_compatible(&database).unwrap().list().unwrap();
            obligations
                .iter()
                .all(|obligation| obligation.state != ObligationState::Running)
                && obligations.iter().any(|obligation| {
                    obligation.failure_disposition == Some(FailureDisposition::Cancelled)
                })
        });
        let obligations = Store::open_compatible(&database).unwrap().list().unwrap();
        assert_eq!(
            obligations
                .iter()
                .filter(|obligation| obligation.failure_disposition
                    == Some(FailureDisposition::Cancelled))
                .count(),
            1
        );
        assert_eq!(
            obligations
                .iter()
                .filter(|obligation| obligation.state == ObligationState::Pending)
                .count(),
            2
        );
    }

    #[test]
    fn backlogged_lane_workers_progress_while_the_other_lane_is_blocked() {
        for blocked_lane in [ExecutionLane::Gardener, ExecutionLane::Ordinary] {
            let other_lane = match blocked_lane {
                ExecutionLane::Gardener => ExecutionLane::Ordinary,
                ExecutionLane::Ordinary => ExecutionLane::Gardener,
                ExecutionLane::Outbox => unreachable!(),
            };
            let barrier = Arc::new(Barrier::new(2));
            let admission = ClaimAdmission::new();
            let (exit_sender, exit_receiver) = mpsc::channel();
            let (progress_sender, progress_receiver) = mpsc::channel();
            let blocked_barrier = Arc::clone(&barrier);
            let blocked = spawn_worker(
                ExecutionWorkerId {
                    lane: blocked_lane,
                    slot: 1,
                },
                exit_sender.clone(),
                admission.clone(),
                move || {
                    blocked_barrier.wait();
                    blocked_barrier.wait();
                    Ok(())
                },
            )
            .unwrap();
            barrier.wait();
            let progressing = spawn_worker(
                ExecutionWorkerId {
                    lane: other_lane,
                    slot: 1,
                },
                exit_sender,
                admission,
                move || {
                    for sequence in 0..3 {
                        progress_sender.send((other_lane, sequence)).unwrap();
                    }
                    Ok(())
                },
            )
            .unwrap();

            for sequence in 0..3 {
                assert_eq!(
                    progress_receiver
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap(),
                    (other_lane, sequence),
                    "a backlogged lane must progress independently of blocked work in the other"
                );
            }
            barrier.wait();
            for _ in 0..2 {
                assert!(exit_receiver.recv_timeout(Duration::from_secs(1)).is_ok());
            }
            blocked.thread.join().unwrap();
            progressing.thread.join().unwrap();
        }
    }

    #[test]
    fn saturated_store_claimants_alternate_lane_turns_in_both_directions() {
        assert_store_claims_alternate_after(ExecutionLane::Ordinary);
        assert_store_claims_alternate_after(ExecutionLane::Gardener);
    }

    #[test]
    fn worker_panics_are_reported_with_their_lane_identity() {
        let (sender, receiver) = mpsc::channel();
        let admission = ClaimAdmission::new();
        let worker = spawn_worker(
            ExecutionWorkerId {
                lane: ExecutionLane::Gardener,
                slot: 1,
            },
            sender,
            admission.clone(),
            || -> Result<(), LaneFailureCause> { panic!("test lane crash") },
        )
        .unwrap();
        let exit = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(exit.id.lane, ExecutionLane::Gardener);
        assert!(matches!(exit.result, Err(LaneFailureCause::Panicked)));
        assert!(admission.is_closed());
        worker.thread.join().unwrap();
    }

    #[test]
    fn closing_admission_prevents_a_claim_waiting_for_the_gate() {
        let admission = ClaimAdmission::new();
        let (active_sender, active_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let active_admission = admission.clone();
        let active = thread::spawn(move || {
            active_admission
                .claim(ExecutionLane::Ordinary, || {
                    active_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                    Ok::<_, ()>(())
                })
                .unwrap()
        });
        active_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (waiting_sender, waiting_receiver) = mpsc::channel();
        let (claim_sender, claim_receiver) = mpsc::channel();
        let (executed_sender, executed_receiver) = mpsc::channel();
        let claimant_admission = admission.clone();
        let claimant = thread::spawn(move || {
            waiting_sender.send(()).unwrap();
            let result = claimant_admission.claim(ExecutionLane::Gardener, || {
                executed_sender.send(()).unwrap();
                Ok::<_, ()>(())
            });
            claim_sender.send(result).unwrap();
        });
        waiting_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        wait_until(Duration::from_secs(1), || {
            admission
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .waiting[1]
                == 1
        });

        let closer_admission = admission.clone();
        let closer = thread::spawn(move || closer_admission.close());
        wait_until(Duration::from_secs(1), || admission.is_closed());
        release_sender.send(()).unwrap();

        assert_eq!(active.join().unwrap(), Some(()));
        assert_eq!(
            claim_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Ok(None)
        );
        assert!(executed_receiver.try_recv().is_err());
        claimant.join().unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn store_failure_stops_admission_and_returns_a_typed_lane_cause() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("failure.sqlite");
        let mut store = Store::open(&database).unwrap();
        let scheduler = Scheduler::start(config(database.clone(), 1, Duration::ZERO)).unwrap();
        let admission = scheduler.admission();

        let connection = rusqlite::Connection::open(&database).unwrap();
        connection.execute_batch("DROP TABLE attempts;").unwrap();
        drop(connection);
        create_due(&mut store, "trigger-failed-claim");
        drop(store);

        wait_until(Duration::from_secs(2), || admission.is_closed());
        let error = scheduler.shutdown().unwrap_err();
        match error {
            SchedulerError::Shutdown(failure) => {
                let primary = failure.primary.as_ref().expect("primary failure");
                assert_eq!(primary.lane, ExecutionLane::Ordinary);
                assert!(matches!(primary.cause.as_ref(), LaneFailureCause::Store(_)));
                assert!(failure.timed_out.is_empty());
            }
            other => panic!("unexpected scheduler result: {other}"),
        }
        let mut claim_executed = false;
        let result = admission
            .claim(ExecutionLane::Ordinary, || {
                claim_executed = true;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(result, None);
        assert!(!claim_executed, "a failed lane must leave admission closed");
    }

    #[test]
    fn shutdown_error_retains_primary_failure_and_every_timed_out_worker() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("aggregate-shutdown.sqlite");
        let mut store = Store::open(&database).unwrap();
        create_due(&mut store, "leased-one");
        create_due(&mut store, "leased-two");
        drop(store);

        let admission = ClaimAdmission::new();
        let (exit_sender, exit_receiver) = mpsc::channel();
        let (active_sender, active_receiver) = mpsc::channel();
        let (done_sender, done_receiver) = mpsc::channel();
        let release = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for slot in 1..=2 {
            let worker_database = database.clone();
            let worker_admission = admission.clone();
            let worker_active_sender = active_sender.clone();
            let worker_done_sender = done_sender.clone();
            let worker_release = Arc::clone(&release);
            let id = ExecutionWorkerId {
                lane: ExecutionLane::Ordinary,
                slot,
            };
            workers.push(
                spawn_worker(id, exit_sender.clone(), admission.clone(), move || {
                    let mut store = Store::open_compatible(worker_database)?;
                    let claims = worker_admission
                        .claim(ExecutionLane::Ordinary, || {
                            store.claim_due(SystemClock.now(), 30, 1)
                        })?
                        .expect("admission is open");
                    assert_eq!(claims.len(), 1);
                    worker_active_sender.send(()).unwrap();
                    worker_release.wait();
                    worker_done_sender.send(()).unwrap();
                    Ok(())
                })
                .unwrap(),
            );
        }
        for _ in 0..2 {
            active_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
        }

        let failing_id = ExecutionWorkerId {
            lane: ExecutionLane::Gardener,
            slot: 1,
        };
        workers.push(
            spawn_worker(failing_id, exit_sender, admission.clone(), || {
                Err(LaneFailureCause::Gardener(
                    GardenerRunnerError::Configuration("supervisor fixture failure".to_owned()),
                ))
            })
            .unwrap(),
        );
        let (notify_sender, _notify_receiver) = oneshot::channel();
        let error = supervise_workers(
            workers,
            exit_receiver,
            admission,
            notify_sender,
            None,
            Duration::from_millis(50),
        )
        .unwrap_err();

        let failure = match &error {
            SchedulerError::Shutdown(failure) => failure,
            other => panic!("unexpected supervisor result: {other}"),
        };
        let primary = failure.primary.as_ref().expect("primary lane failure");
        assert_eq!(primary.lane, ExecutionLane::Gardener);
        assert!(matches!(
            primary.cause.as_ref(),
            LaneFailureCause::Gardener(GardenerRunnerError::Configuration(_))
        ));
        assert_eq!(
            failure.timed_out,
            vec![
                ExecutionWorkerId {
                    lane: ExecutionLane::Ordinary,
                    slot: 1,
                },
                ExecutionWorkerId {
                    lane: ExecutionLane::Ordinary,
                    slot: 2,
                },
            ]
        );
        let message = error.to_string();
        assert!(message.contains("primary gardener failure"));
        assert!(message.contains("ordinary[1]"));
        assert!(message.contains("ordinary[2]"));

        let leased = Store::open_compatible(&database).unwrap().list().unwrap();
        assert_eq!(
            leased
                .iter()
                .filter(|obligation| {
                    obligation.state == ObligationState::Running
                        && obligation.lease_expires_at.is_some()
                })
                .count(),
            2,
            "detached workers leave their already-durable claims visibly leased"
        );

        release.wait();
        for _ in 0..2 {
            done_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }

    #[test]
    fn invalid_capacity_is_rejected_before_the_database_is_opened() {
        let temporary = TempDir::new().unwrap();
        let database = temporary.path().join("must-not-be-created.sqlite");
        for ordinary_concurrency in [0, MAX_ORDINARY_CONCURRENCY + 1] {
            let error = match Scheduler::start(config(
                database.clone(),
                ordinary_concurrency,
                Duration::ZERO,
            )) {
                Ok(_) => panic!("invalid capacity unexpectedly started"),
                Err(error) => error,
            };
            assert!(matches!(error, SchedulerError::Configuration(_)));
        }
        assert!(!database.exists());
    }

    #[test]
    fn execution_lane_serialisation_keeps_the_future_outbox_identity_explicit() {
        assert_eq!(
            serde_json::to_string(&ExecutionLane::Outbox).unwrap(),
            "\"outbox\""
        );
        assert_eq!(ExecutionLane::Gardener.to_string(), "gardener");
    }
}
