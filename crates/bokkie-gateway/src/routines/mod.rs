//! Routines: prompts a user's Bots run on a schedule, in the user's time
//! zone, each run a turn in the Bot's own conversation.
//!
//! A run is a row in `routine_runs` that moves through its states as the
//! scheduler and the event router see fit:
//!
//! - **Queued.** When a routine falls due, the scheduler's pass
//!   ([`Routines::pass`]) queues a `waiting` run for the latest time it was
//!   due. Times it missed while Bokkie was down are coalesced into that one
//!   run when the latest is at most [`MISFIRE_WINDOW_MS`] ago, and recorded
//!   as one `missed` run when it is longer. A routine whose previous run is
//!   still waiting is coalesced into it; one whose previous run is running
//!   gets a `skipped` run. "Run now" queues a `manual` run the same way.
//! - **Started.** The pass starts each waiting run as a new turn of its
//!   Bot through the conversation service's core
//!   ([`Conversations::start_routine`]), never steering: it waits while the
//!   Bot is mid-turn, up to [`BUSY_WAIT_MS`], and then is `skipped`; it
//!   waits while the household's ChatGPT sign-in is unavailable until
//!   [`MISFIRE_WINDOW_MS`] after it was due, and then is `missed`. A
//!   refused allowance or Bot makes it `failed`. A disabled account's runs
//!   are `missed`. Pausing a routine skips its waiting run, and a
//!   scheduled run of a paused routine never starts. An attempt to start a
//!   run ends it only while it is still waiting ([`end_waiting`]): a run
//!   another attempt started ends with its turn.
//! - **Ended.** The router ends a running run with its turn
//!   ([`end_turn`]): `succeeded`, `failed` or `interrupted`. The pass
//!   interrupts a turn that runs past [`RUN_DEADLINE_MS`]. An app server
//!   that stops fails the user's running runs, and so does a gateway
//!   restart ([`Routines::recover`]).
//!
//! The pass itself only reads and writes the database. Each start, and each
//! deadline's interrupt, runs in a task of its own that holds its run (one
//! task per run at a time), so a user's slow app server holds up no other
//! user's runs, and no later pass.
//!
//! The scheduler passes every [`PASS_INTERVAL`], on "run now", and whenever
//! a Bot turns idle ([`Conversations::idle_count`]). A pass leaves a run
//! that a task still holds to that task, so a start that found the Bot busy
//! passes again once it lets its run go if the Bot turned idle since it
//! looked: the pass the Bot's idle wake-up set off may have left the run to
//! it. It passes again only then, so a Bot that stays busy is not tried
//! over and over.
//!
//! Every change to a run goes out as [`Event::RoutineRunUpdated`] through
//! [`emit_run`]. A run's end, wherever it happens, also queues its push
//! ([`crate::push::run_ended`]). Three failed runs in a row pause the
//! routine ([`store::count_outcome`]).

pub mod schedule;
pub mod store;

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bokkie_codex::protocol::TurnStatus;
use bokkie_protocol::api::{
    CreateRoutineRequest, Routine, RoutineRun, RunState, RunTrigger, UpdateRoutineRequest,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, RoutineId, RoutineRunId, Timestamp, UserId};
use sqlx::SqlitePool;

use self::store::{NewRoutine, NewRun, RoutineRow, RunRow, Waiting};
use crate::accounts::{self, Account};
use crate::bots::Conversations;
use crate::bots::service::RoutineStart;
use crate::clock::{Clock, HOUR_MS, MINUTE_MS};
use crate::error::{Failure, Result};
use crate::events::{Batch, EventLog};

/// How long after it was due a run may still run late.
pub const MISFIRE_WINDOW_MS: i64 = 6 * HOUR_MS;
/// How long a run waits for its Bot to finish a turn before it is skipped.
pub const BUSY_WAIT_MS: i64 = 30 * MINUTE_MS;
/// How long a run's turn may take before it is interrupted.
pub const RUN_DEADLINE_MS: i64 = 15 * MINUTE_MS;
/// How long an interrupted run's turn has to confirm it ended before the
/// run is ended anyway.
pub const INTERRUPT_GRACE_MS: i64 = MINUTE_MS;
/// How often the scheduler looks for due routines and waiting runs.
pub const PASS_INTERVAL: Duration = Duration::from_secs(10);

/// Why runs end as they do, as their owner reads it.
pub mod reasons {
    pub const DISABLED: &str = "Your account is disabled";
    pub const DOWN: &str = "Bokkie was not running when it was due";
    pub const SIGNED_OUT: &str = "ChatGPT was signed out for the household";
    pub const WAITING_SIGNED_OUT: &str = "Waiting for the household's ChatGPT sign-in";
    pub const BUSY: &str = "The Bot was busy for 30 minutes";
    pub const WAITING_BUSY: &str = "Waiting for the Bot to finish its turn";
    pub const OVERLAP: &str = "The routine's previous run was still going";
    pub const DEADLINE: &str = "The run took longer than 15 minutes, so it was stopped";
    pub const STOPPED: &str = "Stopped before it finished";
    pub const OUT_OF_TOKENS: &str = "Your token allowance ran out, so the run was stopped";
    pub const TURN_FAILED: &str = "The Bot's turn failed";
    pub const RESTARTED: &str = "Bokkie restarted during the run";
    pub const PAUSED: &str = "The routine was paused before the run started";
}

/// Announces a run as it now is, with the messages it added to the
/// conversation. Every queued, started and ended run goes through here.
pub async fn emit_run(batch: &mut Batch, run: &RunRow) -> Result<()> {
    let messages = store::run_messages(&mut **batch, run).await?;
    batch
        .emit(Event::RoutineRunUpdated {
            run: run.to_api(messages),
        })
        .await
}

/// Ends a waiting or running run in `state`, announces it, and counts its
/// outcome against its routine: a failure, or a turn stopped for running
/// too long, adds to the routine's failures and may pause it; a success
/// clears them. Returns the run if it had not already ended.
pub async fn end_run(
    batch: &mut Batch,
    now: Timestamp,
    run: &RunRow,
    state: RunState,
    reason: Option<&str>,
) -> Result<Option<RunRow>> {
    let Some(ended) = store::finish(&mut **batch, now, run.user, run.id, state, reason).await?
    else {
        return Ok(None);
    };
    announce_end(batch, now, &ended, state).await?;
    Ok(Some(ended))
}

/// Ends a run in `state` as [`end_run`] does, but only if it is still
/// waiting: a run that started in the meantime is left to end with its
/// turn. Returns the run if it was waiting.
pub async fn end_waiting(
    batch: &mut Batch,
    now: Timestamp,
    run: &RunRow,
    state: RunState,
    reason: Option<&str>,
) -> Result<Option<RunRow>> {
    let Some(ended) =
        store::finish_waiting(&mut **batch, now, run.user, run.id, state, reason).await?
    else {
        return Ok(None);
    };
    announce_end(batch, now, &ended, state).await?;
    Ok(Some(ended))
}

/// Announces a run that just ended in `state`, counts its outcome, and
/// queues its push ([`crate::push::run_ended`]).
async fn announce_end(
    batch: &mut Batch,
    now: Timestamp,
    ended: &RunRow,
    state: RunState,
) -> Result<()> {
    emit_run(batch, ended).await?;
    let failed = match state {
        RunState::Failed => Some(true),
        RunState::Interrupted if ended.deadline_passed_at.is_some() => Some(true),
        RunState::Succeeded => Some(false),
        _ => None,
    };
    let mut paused = false;
    if let Some(failed) = failed
        && let Some((routine, paused_now)) = store::count_outcome(batch, now, ended, failed).await?
    {
        paused = paused_now;
        batch
            .emit(Event::RoutineUpdated {
                routine: routine.to_api(),
            })
            .await?;
    }
    crate::push::run_ended(batch, now, ended, state, paused).await
}

/// Ends the run of one of the user's turns that ended with `status`, if a
/// routine ran it and it is still running. `out_of_tokens` says the
/// gateway interrupted the turn because the user's token allowance ran
/// out.
pub async fn end_turn(
    batch: &mut Batch,
    now: Timestamp,
    user: UserId,
    turn_id: &str,
    status: TurnStatus,
    out_of_tokens: bool,
) -> Result<()> {
    let Some(run) = store::run_of_turn(&mut **batch, user, turn_id).await? else {
        return Ok(());
    };
    if run.state != RunState::Running {
        return Ok(());
    }
    let (state, reason) = match status {
        TurnStatus::Completed | TurnStatus::InProgress => (RunState::Succeeded, None),
        TurnStatus::Interrupted if run.deadline_passed_at.is_some() => {
            (RunState::Interrupted, Some(reasons::DEADLINE))
        }
        TurnStatus::Interrupted if out_of_tokens => {
            (RunState::Interrupted, Some(reasons::OUT_OF_TOKENS))
        }
        TurnStatus::Interrupted => (RunState::Interrupted, Some(reasons::STOPPED)),
        TurnStatus::Failed => (RunState::Failed, Some(reasons::TURN_FAILED)),
    };
    end_run(batch, now, &run, state, reason).await?;
    Ok(())
}

/// Ends the user's running runs, of one Bot or all of them, as when their
/// app server stops or a Bot is refused. A disabled account's runs say so.
pub async fn end_running(
    batch: &mut Batch,
    now: Timestamp,
    user: UserId,
    bot: Option<BotId>,
    state: RunState,
    reason: &str,
) -> Result<()> {
    let disabled = accounts::account(&mut **batch, user)
        .await?
        .is_some_and(|a| a.user.disabled);
    let reason = if disabled { reasons::DISABLED } else { reason };
    for run in store::running_runs_of(&mut **batch, user, bot).await? {
        end_run(batch, now, &run, state, Some(reason)).await?;
    }
    Ok(())
}

/// The routine service: routines through the API, and the scheduler.
/// Clones share the scheduler's state.
#[derive(Debug, Clone)]
pub struct Routines {
    db: SqlitePool,
    clock: Clock,
    events: Arc<EventLog>,
    conversations: Arc<Conversations>,
    /// Woken by "run now", so the scheduler starts the run at once, and by
    /// a start that found its Bot busy and missed it turning idle.
    wake: Arc<tokio::sync::Notify>,
    tasks: Arc<Tasks>,
    #[cfg(feature = "test-network")]
    seams: Arc<seams::Seams>,
}

/// The runs the scheduler's tasks hold: at most one task per run.
#[derive(Debug, Default)]
struct Tasks {
    runs: std::sync::Mutex<HashSet<RoutineRunId>>,
    /// Woken whenever a task lets its run go.
    released: tokio::sync::Notify,
}

impl Tasks {
    fn runs(&self) -> std::sync::MutexGuard<'_, HashSet<RoutineRunId>> {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits until `holding` is false of the runs held.
    async fn until_not(&self, holding: impl Fn(&HashSet<RoutineRunId>) -> bool) {
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if !holding(&self.runs()) {
                return;
            }
            released.await;
        }
    }
}

/// A task's hold on its run, let go however the task ends.
struct Claim {
    tasks: Arc<Tasks>,
    run: RoutineRunId,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.tasks.runs().remove(&self.run);
        self.tasks.released.notify_waiters();
    }
}

impl Routines {
    pub fn new(
        db: SqlitePool,
        clock: Clock,
        events: Arc<EventLog>,
        conversations: Arc<Conversations>,
    ) -> Self {
        Self {
            db,
            clock,
            events,
            conversations,
            wake: Default::default(),
            tasks: Default::default(),
            #[cfg(feature = "test-network")]
            seams: Default::default(),
        }
    }

    /// The scheduler's test seams. Only the `test-network` feature builds
    /// them.
    #[cfg(feature = "test-network")]
    pub fn seams(&self) -> &seams::Seams {
        &self.seams
    }

    // -- The API ------------------------------------------------------------

    /// Creates a routine on one of the user's Bots, and announces it.
    pub async fn create(
        &self,
        user: UserId,
        bot: BotId,
        request: CreateRoutineRequest,
    ) -> Result<Routine> {
        let mut batch = self.events.begin(user).await?;
        let routine = self
            .create_in(&mut batch, self.clock.now(), user, bot, &request)
            .await?;
        batch.commit().await?;
        tracing::info!(user = %user, bot = %bot, routine = %routine.id, "created a routine");
        Ok(routine)
    }

    /// Creates a routine in the caller's batch and announces it there: the
    /// checks of [`Routines::create`], the routine limit counting what the
    /// batch has made so far.
    pub(crate) async fn create_in(
        &self,
        batch: &mut Batch,
        now: Timestamp,
        user: UserId,
        bot: BotId,
        request: &CreateRoutineRequest,
    ) -> Result<Routine> {
        let name = store::check_name(&request.name)?;
        let prompt = store::check_prompt(&request.prompt)?;
        schedule::check(&request.schedule)?;
        let account = own_account(batch, user).await?;
        crate::bots::store::bot(&mut **batch, user, bot)
            .await?
            .ok_or_else(Failure::not_found)?;
        if store::count(&mut **batch, user).await? >= store::MAX_ROUTINES {
            return Err(Failure::conflict(format!(
                "You can have at most {} routines",
                store::MAX_ROUTINES
            )));
        }
        let zone = schedule::stored_zone(&account.user.time_zone);
        let next_run_at = if request.paused {
            None
        } else {
            schedule::next_after(&request.schedule, &zone, now)
        };
        let routine = store::create(
            &mut **batch,
            now,
            user,
            NewRoutine {
                bot,
                name: &name,
                prompt: &prompt,
                schedule: &request.schedule,
                push: request.push,
                paused: request.paused,
                next_run_at,
            },
        )
        .await?
        .to_api();
        batch
            .emit(Event::RoutineCreated {
                routine: routine.clone(),
            })
            .await?;
        Ok(routine)
    }

    /// Changes one of the user's routines, and announces it. A new
    /// schedule, or resuming, schedules it from now; resuming also clears
    /// its failures.
    pub async fn update(
        &self,
        user: UserId,
        bot: BotId,
        id: RoutineId,
        request: UpdateRoutineRequest,
    ) -> Result<Routine> {
        // Pausing waits for any start of a run of the Bot, which holds the
        // Bot's lock, so that a run starts either before the pause or not
        // at all.
        let _starts = match request.paused {
            Some(true) => Some(self.conversations.lock_bot(user, bot).await),
            _ => None,
        };
        let mut batch = self.events.begin(user).await?;
        let routine = self
            .update_in(&mut batch, self.clock.now(), user, bot, id, request)
            .await?;
        batch.commit().await?;
        tracing::info!(user = %user, bot = %bot, routine = %id, paused = routine.paused, "updated a routine");
        Ok(routine)
    }

    /// Changes a routine in the caller's batch and announces it there, as
    /// [`Routines::update`] does. A caller that pauses the routine holds
    /// the Bot's lock ([`Conversations::lock_bot`]) first.
    pub(crate) async fn update_in(
        &self,
        batch: &mut Batch,
        now: Timestamp,
        user: UserId,
        bot: BotId,
        id: RoutineId,
        request: UpdateRoutineRequest,
    ) -> Result<Routine> {
        let name = request.name.as_deref().map(store::check_name).transpose()?;
        let prompt = request
            .prompt
            .as_deref()
            .map(store::check_prompt)
            .transpose()?;
        if let Some(schedule) = &request.schedule {
            schedule::check(schedule)?;
        }
        let account = own_account(batch, user).await?;
        let mut routine = store::routine(&mut **batch, user, bot, id)
            .await?
            .ok_or_else(Failure::not_found)?;
        let was_paused = routine.paused;
        if let Some(name) = name {
            routine.name = name;
        }
        if let Some(prompt) = prompt {
            routine.prompt = prompt;
        }
        if let Some(push) = request.push {
            routine.push = push;
        }
        let rescheduled = request.schedule.is_some();
        if let Some(schedule) = request.schedule {
            routine.schedule = schedule;
        }
        if let Some(paused) = request.paused {
            routine.paused = paused;
        }
        if routine.paused {
            routine.next_run_at = None;
        } else if was_paused || rescheduled {
            if was_paused {
                routine.failures = 0;
            }
            let zone = schedule::stored_zone(&account.user.time_zone);
            routine.next_run_at = schedule::next_after(&routine.schedule, &zone, now);
        }
        let routine = store::save(&mut **batch, now, &routine).await?.to_api();
        batch
            .emit(Event::RoutineUpdated {
                routine: routine.clone(),
            })
            .await?;
        // A pause skips the run that waits to start.
        if routine.paused
            && !was_paused
            && let Some(waiting) = store::active_run(&mut **batch, user, id).await?
        {
            end_waiting(
                batch,
                now,
                &waiting,
                RunState::Skipped,
                Some(reasons::PAUSED),
            )
            .await?;
        }
        Ok(routine)
    }

    /// Deletes one of the user's routines with its history, and announces
    /// it. A run of it that is running carries on as an ordinary turn.
    pub async fn delete(&self, user: UserId, bot: BotId, id: RoutineId) -> Result<()> {
        let mut batch = self.events.begin(user).await?;
        delete_in(&mut batch, user, bot, id).await?;
        batch.commit().await?;
        tracing::info!(user = %user, bot = %bot, routine = %id, "deleted a routine");
        Ok(())
    }

    /// "Run now": queues a manual run of one of the user's routines, paused
    /// or not, exactly as the schedule would, and tries to start it at
    /// once. Returns the run as it then is.
    pub async fn run_now(&self, user: UserId, bot: BotId, id: RoutineId) -> Result<RoutineRun> {
        let now = self.clock.now();
        let mut batch = self.events.begin(user).await?;
        let routine = store::routine(&mut *batch, user, bot, id)
            .await?
            .ok_or_else(Failure::not_found)?;
        let account = own_account(&mut batch, user).await?;
        let run = self
            .queue(&mut batch, now, &account, &routine, RunTrigger::Manual, now)
            .await?;
        batch.commit().await?;
        tracing::info!(user = %user, bot = %bot, routine = %id, "running a routine now");
        let Some(run) = run else {
            // Coalesced into the waiting run.
            let run = store::active_run(&self.db, user, id)
                .await?
                .ok_or_else(Failure::not_found)?;
            return self.to_api(&run).await;
        };
        if run.state == RunState::Waiting {
            // In a task of its own, as the scheduler starts it, so that it
            // starts once, and whole however this request ends.
            self.spawn_start(&run, now);
            self.tasks.until_not(|runs| runs.contains(&run.id)).await;
            self.wake.notify_one();
        }
        let run = store::run(&self.db, user, run.id)
            .await?
            .ok_or_else(Failure::not_found)?;
        self.to_api(&run).await
    }

    /// Sets the user's time zone, reschedules their routines in it, and
    /// announces the account and each rescheduled routine.
    pub async fn set_time_zone(&self, user: UserId, time_zone: &str) -> Result<Account> {
        let time_zone = schedule::check_time_zone(time_zone)?;
        let zone = schedule::stored_zone(&time_zone);
        let now = self.clock.now();
        let mut batch = self.events.begin(user).await?;
        let account = accounts::set_time_zone(&mut batch, now, user, &time_zone).await?;
        batch
            .emit(Event::AccountUpdated {
                user: account.user.clone(),
            })
            .await?;
        for mut routine in store::list_for_user(&mut *batch, user).await? {
            if routine.paused {
                continue;
            }
            routine.next_run_at = schedule::next_after(&routine.schedule, &zone, now);
            let routine = store::save(&mut *batch, now, &routine).await?;
            batch
                .emit(Event::RoutineUpdated {
                    routine: routine.to_api(),
                })
                .await?;
        }
        batch.commit().await?;
        tracing::info!(user = %user, time_zone, "set a time zone");
        Ok(account)
    }

    /// A run as its owner sees it.
    pub async fn to_api(&self, run: &RunRow) -> Result<RoutineRun> {
        let messages = store::run_messages(&self.db, run).await?;
        Ok(run.to_api(messages))
    }

    // -- The scheduler ------------------------------------------------------

    /// Ends the runs a previous gateway left running: their turns ended
    /// with it. Waiting runs carry on.
    pub async fn recover(&self) -> Result<()> {
        let now = self.clock.now();
        let running = store::active_runs(&self.db, RunState::Running).await?;
        for run in &running {
            let mut batch = self.events.begin(run.user).await?;
            end_run(
                &mut batch,
                now,
                run,
                RunState::Failed,
                Some(reasons::RESTARTED),
            )
            .await?;
            batch.commit().await?;
        }
        if !running.is_empty() {
            tracing::warn!(count = running.len(), "ended routine runs left running");
        }
        Ok(())
    }

    /// Runs the scheduler's pass every `every`, and as soon as a Bot's turn
    /// ends or a run is asked for, until the gateway stops.
    pub async fn run(self: Arc<Self>, every: Duration) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.wake.notified() => {}
                _ = self.conversations.idle.notified() => {}
            }
            if let Err(failure) = self.pass(self.clock.now()).await {
                tracing::error!(message = %failure.message, "the routine scheduler's pass failed");
            }
        }
    }

    /// One pass of the scheduler at `now`: queues the runs of due routines,
    /// and sets off starting the waiting runs and enforcing the running
    /// runs' deadlines, each in a task of its own that holds its run. It
    /// waits on no app server; a run whose task from an earlier pass is
    /// still going is left to that task.
    pub async fn pass(&self, now: Timestamp) -> Result<()> {
        for routine in store::due(&self.db, now).await? {
            if let Err(failure) = self.fall_due(&routine, now).await {
                tracing::error!(user = %routine.user, routine = %routine.id, message = %failure.message, "queuing a routine's run failed");
            }
        }
        for run in store::active_runs(&self.db, RunState::Waiting).await? {
            self.spawn_start(&run, now);
        }
        for run in store::active_runs(&self.db, RunState::Running).await? {
            let this = self.clone();
            let held = run.clone();
            self.spawn_holding(
                &run,
                "stopping a routine's run",
                async move { this.enforce_deadline(&held, now).await },
                |()| {},
            );
        }
        #[cfg(feature = "test-network")]
        self.seams.passed();
        Ok(())
    }

    /// Waits until every task the scheduler set off has finished, such as
    /// a pass's starts.
    pub async fn settle(&self) {
        self.tasks.until_not(|runs| !runs.is_empty()).await;
    }

    /// Sets off starting a waiting run, unless a task holds it already. If
    /// the attempt finds the Bot busy, and the Bot turns idle before the
    /// task lets the run go, the scheduler passes again then: a pass the
    /// Bot's idle wake-up set off meanwhile left the run to this task.
    fn spawn_start(&self, run: &RunRow, now: Timestamp) {
        let this = self.clone();
        let after = self.clone();
        let held = run.clone();
        let bot = run.bot;
        self.spawn_holding(
            run,
            "starting a routine's run",
            async move {
                // Counted before the attempt can find the Bot busy.
                let idled = this.conversations.idle_count(held.bot);
                let busy = this.attempt(&held, now).await?;
                #[cfg(feature = "test-network")]
                if busy {
                    this.seams.found_busy().await;
                }
                Ok(busy.then_some(idled))
            },
            move |busy_since| {
                if let Some(idled) = busy_since
                    && after.conversations.idle_count(bot) != idled
                {
                    after.wake.notify_one();
                }
            },
        );
    }

    /// Runs `work` in a task that holds `run` until it finishes, unless
    /// another task holds it already, then hands what it did to `released`
    /// once the run is let go.
    fn spawn_holding<T: Send + 'static>(
        &self,
        run: &RunRow,
        doing: &'static str,
        work: impl Future<Output = Result<T>> + Send + 'static,
        released: impl FnOnce(T) + Send + 'static,
    ) {
        if !self.tasks.runs().insert(run.id) {
            return;
        }
        let claim = Claim {
            tasks: self.tasks.clone(),
            run: run.id,
        };
        let (user, id) = (run.user, run.id);
        tokio::spawn(async move {
            let done = {
                let _claim = claim;
                work.await
            };
            match done {
                Ok(done) => released(done),
                Err(failure) => {
                    tracing::error!(user = %user, run = %id, message = %failure.message, "{doing} failed");
                }
            }
        });
    }

    /// Queues the run of a routine that fell due, and schedules its next.
    async fn fall_due(&self, due: &RoutineRow, now: Timestamp) -> Result<()> {
        let mut batch = self.events.begin(due.user).await?;
        // Read again under the user's lock: an edit may have come between.
        let Some(mut routine) = store::routine_of_user(&mut *batch, due.user, due.id).await? else {
            return Ok(());
        };
        let Some(next_run_at) = routine
            .next_run_at
            .filter(|at| *at <= now && !routine.paused)
        else {
            return Ok(());
        };
        let account = own_account(&mut batch, routine.user).await?;
        let zone = schedule::stored_zone(&account.user.time_zone);
        // Times missed since are coalesced into the latest.
        let latest = schedule::latest_at_or_before(&routine.schedule, &zone, now)
            .filter(|latest| *latest >= next_run_at)
            .unwrap_or(next_run_at);
        routine.next_run_at = schedule::next_after(&routine.schedule, &zone, now);
        let routine = store::save(&mut *batch, now, &routine).await?;
        batch
            .emit(Event::RoutineUpdated {
                routine: routine.to_api(),
            })
            .await?;
        self.queue(
            &mut batch,
            now,
            &account,
            &routine,
            RunTrigger::Schedule,
            latest,
        )
        .await?;
        batch.commit().await?;
        Ok(())
    }

    /// Records a run of `routine` due at `scheduled_for`: `missed` if it
    /// cannot run, `skipped` if its previous run is running, coalesced into
    /// its previous run if that is still waiting, or else `waiting`.
    /// Returns the run recorded, if any.
    async fn queue(
        &self,
        batch: &mut Batch,
        now: Timestamp,
        account: &Account,
        routine: &RoutineRow,
        trigger: RunTrigger,
        scheduled_for: Timestamp,
    ) -> Result<Option<RunRow>> {
        let (state, reason) = if account.user.disabled {
            (RunState::Missed, Some(reasons::DISABLED))
        } else if now.0 - scheduled_for.0 > MISFIRE_WINDOW_MS {
            (RunState::Missed, Some(reasons::DOWN))
        } else {
            match store::active_run(&mut **batch, routine.user, routine.id).await? {
                Some(active) if active.state == RunState::Waiting => {
                    // The waiting run stands for this one too, due now.
                    if trigger == RunTrigger::Schedule
                        && let Some(moved) = store::reschedule_waiting(
                            &mut **batch,
                            routine.user,
                            active.id,
                            scheduled_for,
                        )
                        .await?
                    {
                        emit_run(batch, &moved).await?;
                    }
                    return Ok(None);
                }
                Some(_) => (RunState::Skipped, Some(reasons::OVERLAP)),
                None => (RunState::Waiting, None),
            }
        };
        let run = store::insert_run(
            &mut **batch,
            now,
            routine,
            NewRun {
                trigger,
                scheduled_for,
                state,
                reason,
            },
        )
        .await?;
        emit_run(batch, &run).await?;
        if state == RunState::Missed {
            crate::push::run_ended(batch, now, &run, state, false).await?;
        }
        tracing::info!(user = %routine.user, routine = %routine.id, run = %run.id, state = store::state_name(state), "queued a routine's run");
        Ok(Some(run))
    }

    /// Tries to start a waiting run, as the scheduler's tasks do. `run` may
    /// be out of date: an attempt that does not start it ends it only if it
    /// is still waiting.
    pub async fn try_start(&self, run: &RunRow, now: Timestamp) -> Result<()> {
        self.attempt(run, now).await.map(drop)
    }

    /// The core of [`Routines::try_start`]. Returns whether the run was
    /// left waiting for its Bot to be idle.
    async fn attempt(&self, run: &RunRow, now: Timestamp) -> Result<bool> {
        #[cfg(feature = "test-network")]
        self.seams.attempted();
        let user = run.user;
        let Some(account) = accounts::account(&self.db, user).await? else {
            return Ok(false);
        };
        if account.user.disabled {
            return self
                .end_unstarted(run, now, RunState::Missed, Some(reasons::DISABLED))
                .await
                .map(|()| false);
        }
        let Some(routine) = store::routine_of_user(&self.db, user, run.routine).await? else {
            return Ok(false);
        };
        let zone = schedule::stored_zone(&account.user.time_zone);
        let when = schedule::local_time(&zone, run.scheduled_for);
        let (marker, about) = match run.trigger {
            RunTrigger::Schedule => (
                format!("{} · {when}", routine.name),
                format!(
                    "This turn is a run of your routine \"{}\", scheduled for {}. Its prompt, \
                     which {} set, is the message. {} reads your reply later.",
                    routine.name,
                    schedule::local_description(&zone, run.scheduled_for),
                    account.user.display_name,
                    account.user.display_name,
                ),
            ),
            RunTrigger::Manual => (
                format!("{} · {when} · test run", routine.name),
                format!(
                    "This turn is a test run of your routine \"{}\", which {} started at {}. \
                     Its prompt, which {} set, is the message. Answer as you would on its \
                     schedule.",
                    routine.name,
                    account.user.display_name,
                    schedule::local_description(&zone, run.scheduled_for),
                    account.user.display_name,
                ),
            ),
        };
        let started = self
            .conversations
            .start_routine(&account, &routine, run.id, &marker, about)
            .await;
        // Whether the run is left waiting for the Bot to be idle.
        let mut waits_for_bot = false;
        match started {
            RoutineStart::Started { turn_id } => {
                tracing::info!(user = %user, run = %run.id, turn = %turn_id, "started a routine's run");
                Ok(())
            }
            RoutineStart::NotWaiting => Ok(()),
            RoutineStart::Paused => {
                self.end_unstarted(run, now, RunState::Skipped, Some(reasons::PAUSED))
                    .await
            }
            RoutineStart::Busy if now.0 - run.queued_at.0 >= BUSY_WAIT_MS => {
                self.end_unstarted(run, now, RunState::Skipped, Some(reasons::BUSY))
                    .await
            }
            RoutineStart::Busy => {
                waits_for_bot = true;
                self.wait(run, Waiting::Busy, reasons::WAITING_BUSY).await
            }
            RoutineStart::SignedOut => {
                if now.0 - run.scheduled_for.0 > MISFIRE_WINDOW_MS {
                    self.end_unstarted(run, now, RunState::Missed, Some(reasons::SIGNED_OUT))
                        .await
                } else {
                    self.wait(run, Waiting::SignedOut, reasons::WAITING_SIGNED_OUT)
                        .await
                }
            }
            RoutineStart::Failed(reason) => {
                tracing::info!(user = %user, run = %run.id, %reason, "a routine's run failed to start");
                self.end_unstarted(run, now, RunState::Failed, Some(&reason))
                    .await
            }
        }?;
        Ok(waits_for_bot)
    }

    /// Interrupts a running run's turn once it passes its deadline, and
    /// ends the run if the turn does not confirm it ended soon after.
    async fn enforce_deadline(&self, run: &RunRow, now: Timestamp) -> Result<()> {
        let Some(started) = run.started_at else {
            return Ok(());
        };
        match run.deadline_passed_at {
            None if now.0 - started.0 >= RUN_DEADLINE_MS => {
                store::set_deadline_passed(&self.db, now, run.user, run.id).await?;
                tracing::info!(user = %run.user, run = %run.id, "a routine's run passed its deadline; interrupting it");
                if let (Some(thread), Some(turn)) = (&run.thread_id, &run.turn_id) {
                    self.conversations.interrupt(run.user, thread, turn).await;
                }
                Ok(())
            }
            Some(asked) if now.0 - asked.0 >= INTERRUPT_GRACE_MS => {
                self.end(run, now, RunState::Interrupted, Some(reasons::DEADLINE))
                    .await
            }
            _ => Ok(()),
        }
    }

    async fn end(
        &self,
        run: &RunRow,
        now: Timestamp,
        state: RunState,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut batch = self.events.begin(run.user).await?;
        // Read again under the user's lock: the run may have moved on.
        let Some(current) = store::run(&mut *batch, run.user, run.id).await? else {
            return Ok(());
        };
        end_run(&mut batch, now, &current, state, reason).await?;
        batch.commit().await?;
        Ok(())
    }

    /// Ends a run that an attempt did not start, if it is still waiting.
    async fn end_unstarted(
        &self,
        run: &RunRow,
        now: Timestamp,
        state: RunState,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut batch = self.events.begin(run.user).await?;
        end_waiting(&mut batch, now, run, state, reason).await?;
        batch.commit().await?;
        Ok(())
    }

    async fn wait(&self, run: &RunRow, waiting: Waiting, reason: &str) -> Result<()> {
        let mut batch = self.events.begin(run.user).await?;
        if let Some(changed) =
            store::set_waiting(&mut *batch, run.user, run.id, waiting, reason).await?
        {
            emit_run(&mut batch, &changed).await?;
        }
        batch.commit().await?;
        Ok(())
    }
}

/// Deletes a routine with its history in the caller's batch, and
/// announces it there, as [`Routines::delete`] does.
pub(crate) async fn delete_in(
    batch: &mut Batch,
    user: UserId,
    bot: BotId,
    id: RoutineId,
) -> Result<()> {
    if !store::delete(&mut **batch, user, bot, id).await? {
        return Err(Failure::not_found());
    }
    batch
        .emit(Event::RoutineDeleted {
            bot_id: bot,
            routine_id: id,
        })
        .await
}

async fn own_account(batch: &mut Batch, user: UserId) -> Result<Account> {
    accounts::account(&mut **batch, user)
        .await?
        .ok_or_else(Failure::unauthenticated)
}

/// The scheduler's test seams: counts of its passes and start attempts, and
/// a hold on a start between finding its Bot busy and letting its run go.
/// Only the `test-network` feature builds this, and only this crate's own
/// tests turn it on, so no gateway binary has it.
#[cfg(feature = "test-network")]
pub mod seams {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use tokio::sync::Semaphore;

    #[derive(Debug, Default)]
    pub struct Seams {
        passes: AtomicUsize,
        attempts: AtomicUsize,
        hold: Mutex<Option<Arc<Hold>>>,
    }

    impl Seams {
        /// How many passes the scheduler has finished.
        pub fn passes(&self) -> usize {
            self.passes.load(Ordering::SeqCst)
        }

        /// How many attempts to start a run have begun.
        pub fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }

        /// Holds the next start that finds its Bot busy, once it has
        /// recorded that its run waits and before its task lets the run
        /// go, until [`Hold::open`].
        pub fn hold_next_busy_start(&self) -> Arc<Hold> {
            let hold = Arc::new(Hold {
                reached: Semaphore::new(0),
                open: Semaphore::new(0),
            });
            *self.hold.lock().unwrap_or_else(PoisonError::into_inner) = Some(hold.clone());
            hold
        }

        pub(super) fn passed(&self) {
            self.passes.fetch_add(1, Ordering::SeqCst);
        }

        pub(super) fn attempted(&self) {
            self.attempts.fetch_add(1, Ordering::SeqCst);
        }

        pub(super) async fn found_busy(&self) {
            let hold = self
                .hold
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(hold) = hold {
                hold.reached.add_permits(1);
                if let Ok(open) = hold.open.acquire().await {
                    open.forget();
                }
            }
        }
    }

    /// A start held by [`Seams::hold_next_busy_start`].
    #[derive(Debug)]
    pub struct Hold {
        reached: Semaphore,
        open: Semaphore,
    }

    impl Hold {
        /// Waits until a start is held.
        pub async fn reached(&self) {
            if let Ok(reached) = self.reached.acquire().await {
                reached.forget();
            }
        }

        /// Lets the held start go on.
        pub fn open(&self) {
            self.open.add_permits(1);
        }
    }
}
