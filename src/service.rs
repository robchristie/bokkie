//! Failure-isolated background execution lanes and deterministic service runner.

use std::{
    io,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        Arc, Mutex,
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
    claim_lock: Mutex<()>,
}

impl ClaimAdmission {
    fn new() -> Self {
        Self {
            inner: Arc::new(ClaimAdmissionInner {
                closed: Arc::new(AtomicBool::new(false)),
                claim_lock: Mutex::new(()),
            }),
        }
    }

    /// Atomically stop claim admission and publish cancellation to active work.
    pub fn close(&self) {
        // Publish closure before waiting for an already-admitted claim. Any
        // later mutex winner observes `closed` and cannot enter its Store call.
        self.inner.closed.store(true, Ordering::SeqCst);
        let _guard = self
            .inner
            .claim_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    fn cancellation_token(&self) -> CancellationToken {
        CancellationToken::from_flag(Arc::clone(&self.inner.closed))
    }

    /// Run one Store claim only if admission remains open. Claim errors and
    /// panics close admission before releasing the claim mutex.
    fn claim<T, E>(&self, claim: impl FnOnce() -> Result<T, E>) -> Result<Option<T>, E> {
        let _guard = self
            .inner
            .claim_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_closed() {
            return Ok(None);
        }
        match panic::catch_unwind(AssertUnwindSafe(claim)) {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(error)) => {
                self.inner.closed.store(true, Ordering::SeqCst);
                Err(error)
            }
            Err(payload) => {
                self.inner.closed.store(true, Ordering::SeqCst);
                panic::resume_unwind(payload)
            }
        }
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
    #[error("{lane} execution lane failed: {cause}")]
    LaneFailed {
        lane: ExecutionLane,
        #[source]
        cause: Box<LaneFailureCause>,
    },
    #[error("{lane} execution lane did not stop within {} seconds", LANE_JOIN_TIMEOUT.as_secs())]
    LaneTimeout { lane: ExecutionLane },
    #[error("could not start scheduler supervisor thread: {0}")]
    Thread(#[source] io::Error),
}

impl SchedulerError {
    pub fn is_configuration(&self) -> bool {
        match self {
            Self::Configuration(_) => true,
            Self::LaneFailed { cause, .. } => matches!(
                cause.as_ref(),
                LaneFailureCause::Gardener(GardenerRunnerError::Configuration(_))
            ),
            Self::LaneTimeout { .. } | Self::Thread(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkerId {
    lane: ExecutionLane,
    slot: usize,
}

struct WorkerExit {
    id: WorkerId,
    result: Result<(), LaneFailureCause>,
}

struct Worker {
    id: WorkerId,
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
        Store::open_compatible(&config.database).map_err(|cause| SchedulerError::LaneFailed {
            lane: ExecutionLane::Ordinary,
            cause: Box::new(cause.into()),
        })?;
        if let Some(runtime) = &gardener {
            runtime
                .validate(config.lease_seconds)
                .map_err(|cause| SchedulerError::LaneFailed {
                    lane: ExecutionLane::Gardener,
                    cause: Box::new(cause.into()),
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
            .map_err(|_| SchedulerError::LaneFailed {
                lane: ExecutionLane::Ordinary,
                cause: Box::new(LaneFailureCause::Panicked),
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
        let id = WorkerId {
            lane: ExecutionLane::Ordinary,
            slot,
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
            let id = WorkerId {
                lane: ExecutionLane::Gardener,
                slot: 0,
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

    let mut exit_sender = Some(exit_sender);
    if first_failure.is_some() {
        let _ = exit_sender.take().expect("exit sender is present").send(());
    }
    let mut stop_deadline = admission
        .is_closed()
        .then(|| Instant::now() + LANE_JOIN_TIMEOUT);

    while !workers.is_empty() {
        if admission.is_closed() && stop_deadline.is_none() {
            stop_deadline = Some(Instant::now() + LANE_JOIN_TIMEOUT);
        }
        let wait = stop_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(100));
        if wait.is_zero() {
            let lane = workers[0].id.lane;
            return terminal_result(first_failure, Some(lane));
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
                    stop_deadline = Some(Instant::now() + LANE_JOIN_TIMEOUT);
                    let _ = exit_sender.take().expect("exit sender is present").send(());
                }
            }
            Err(RecvTimeoutError::Timeout) if stop_deadline.is_none() => {}
            Err(RecvTimeoutError::Timeout) => {
                let lane = workers[0].id.lane;
                return terminal_result(first_failure, Some(lane));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let lane = workers[0].id.lane;
                return Err(SchedulerError::LaneFailed {
                    lane,
                    cause: Box::new(LaneFailureCause::Panicked),
                });
            }
        }
    }

    if let Some(sender) = exit_sender.take() {
        let _ = sender.send(());
    }
    terminal_result(first_failure, None)
}

fn terminal_result(
    failure: Option<(ExecutionLane, LaneFailureCause)>,
    timeout: Option<ExecutionLane>,
) -> Result<(), SchedulerError> {
    if let Some((lane, cause)) = failure {
        Err(SchedulerError::LaneFailed {
            lane,
            cause: Box::new(cause),
        })
    } else if let Some(lane) = timeout {
        Err(SchedulerError::LaneTimeout { lane })
    } else {
        Ok(())
    }
}

fn spawn_worker(
    id: WorkerId,
    sender: mpsc::Sender<WorkerExit>,
    admission: ClaimAdmission,
    work: impl FnOnce() -> Result<(), LaneFailureCause> + Send + 'static,
) -> Result<Worker, LaneFailureCause> {
    let name = format!("bokkie-{}-{}", id.lane, id.slot + 1);
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
        let Some(mut claims) =
            admission.claim(|| store.claim_due(clock.now(), config.lease_seconds, 1))?
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
        let Some(mut claims) =
            admission.claim(|| store.claim_due_gardener(clock.now(), config.lease_seconds, 1))?
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
                WorkerId {
                    lane: blocked_lane,
                    slot: 0,
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
                WorkerId {
                    lane: other_lane,
                    slot: 0,
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
    fn worker_panics_are_reported_with_their_lane_identity() {
        let (sender, receiver) = mpsc::channel();
        let admission = ClaimAdmission::new();
        let worker = spawn_worker(
            WorkerId {
                lane: ExecutionLane::Gardener,
                slot: 0,
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
        let gate = admission
            .inner
            .claim_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (waiting_sender, waiting_receiver) = mpsc::channel();
        let (claim_sender, claim_receiver) = mpsc::channel();
        let (executed_sender, executed_receiver) = mpsc::channel();
        let claimant_admission = admission.clone();
        let claimant = thread::spawn(move || {
            waiting_sender.send(()).unwrap();
            let result = claimant_admission.claim(|| {
                executed_sender.send(()).unwrap();
                Ok::<_, ()>(())
            });
            claim_sender.send(result).unwrap();
        });
        waiting_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let closer_admission = admission.clone();
        let closer = thread::spawn(move || closer_admission.close());
        wait_until(Duration::from_secs(1), || admission.is_closed());
        drop(gate);

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
            SchedulerError::LaneFailed { lane, cause } => {
                assert_eq!(lane, ExecutionLane::Ordinary);
                assert!(matches!(cause.as_ref(), LaneFailureCause::Store(_)));
            }
            other => panic!("unexpected scheduler result: {other}"),
        }
        let mut claim_executed = false;
        let result = admission
            .claim(|| {
                claim_executed = true;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(result, None);
        assert!(!claim_executed, "a failed lane must leave admission closed");
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
