//! Background scheduler and deterministic service runner.

use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::gardener_runner::{GardenerRunner, GardenerRunnerError, GardenerRuntimeConfig};
use crate::{Claim, Completion, RunResult, Runner, Store, StoreError, SystemClock, UnixClock};

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
    pub fake_delay: Duration,
    pub fake_outcome: ServiceFakeOutcome,
}

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("scheduler thread panicked")]
    Panicked,
    #[error("could not start scheduler thread: {0}")]
    Thread(#[source] io::Error),
    #[error(transparent)]
    Gardener(#[from] GardenerRunnerError),
}

pub struct Scheduler {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), SchedulerError>>>,
    exit: Option<oneshot::Receiver<()>>,
}

impl Scheduler {
    pub fn start(config: SchedulerConfig) -> Result<Self, SchedulerError> {
        Self::start_configured(config, None)
    }

    /// Starts the scheduler with an optional coding-gardener execution runtime.
    /// Gardener-bound obligations are not claimable when this is `None`.
    pub fn start_configured(
        config: SchedulerConfig,
        gardener: Option<GardenerRuntimeConfig>,
    ) -> Result<Self, SchedulerError> {
        // Fail before advertising readiness if the database cannot be opened or migrated.
        Store::open(&config.database)?;
        if let Some(runtime) = &gardener {
            runtime.validate(config.lease_seconds)?;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let scheduler_stop = Arc::clone(&stop);
        let (exit_sender, exit) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("bokkie-scheduler".to_owned())
            .spawn(move || {
                let result = scheduler_loop(config, gardener, &scheduler_stop);
                let _ = exit_sender.send(());
                result
            })
            .map_err(SchedulerError::Thread)?;
        Ok(Self {
            stop,
            thread: Some(thread),
            exit: Some(exit),
        })
    }

    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Resolve when the scheduler thread returns or panics.
    pub fn take_exit_signal(&mut self) -> oneshot::Receiver<()> {
        self.exit
            .take()
            .expect("scheduler exit signal can only be taken once")
    }

    /// Stop claiming new work, then wait for already-claimed work to reconcile.
    pub fn shutdown(mut self) -> Result<(), SchedulerError> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread
            .take()
            .expect("scheduler thread is present")
            .join()
            .map_err(|_| SchedulerError::Panicked)??;
        Ok(())
    }
}

fn scheduler_loop(
    config: SchedulerConfig,
    gardener: Option<GardenerRuntimeConfig>,
    stop: &AtomicBool,
) -> Result<(), SchedulerError> {
    let mut store = Store::open(&config.database)?;
    let clock = SystemClock;

    while !stop.load(Ordering::SeqCst) {
        if let Some(runtime) = &gardener {
            let mut claims = store.claim_due_gardener(clock.now(), config.lease_seconds, 1)?;
            if let Some(claim) = claims.pop() {
                let runner = GardenerRunner::new(runtime, config.lease_seconds, &clock)?;
                let result = runner.execute(&mut store, &claim);
                reconcile_completion(&mut store, &claim, result, &clock)?;
                continue;
            }
        }
        let mut claims = store.claim_due(clock.now(), config.lease_seconds, 1)?;
        if let Some(claim) = claims.pop() {
            delay_with_lease_renewal(
                &mut store,
                &claim,
                config.fake_delay,
                config.lease_seconds,
                &clock,
            )?;
            let mut runner = ServiceFakeRunner {
                outcome: config.fake_outcome,
            };
            let result = runner.execute(&claim);
            reconcile_completion(&mut store, &claim, result, &clock)?;
            continue;
        }
        sleep_until_poll_or_stop(config.poll_interval, stop);
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

fn sleep_until_poll_or_stop(interval: Duration, stop: &AtomicBool) {
    let quantum = Duration::from_millis(25);
    let mut remaining = interval;
    while !remaining.is_zero() && !stop.load(Ordering::SeqCst) {
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
) -> Result<(), StoreError> {
    let started = Instant::now();
    let mut lease_expires_at = claim.lease_expires_at;
    while started.elapsed() < delay {
        let now = clock.now();
        if now >= lease_expires_at.saturating_sub(1) {
            lease_expires_at = store.renew_lease(claim, now, lease_seconds)?;
        }
        let remaining = delay.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
    Ok(())
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
                retryable: true,
                error: "deterministic retryable fake failure".to_owned(),
                evidence: Some("fake runner configured to fail retryably".to_owned()),
            },
            ServiceFakeOutcome::FailTerminal => Completion::Failed {
                retryable: false,
                error: "deterministic terminal fake failure".to_owned(),
                evidence: Some("fake runner configured to fail terminally".to_owned()),
            },
        };
        RunResult { completion }
    }
}
