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
//!   are `missed`.
//! - **Ended.** The router ends a running run with its turn
//!   ([`end_turn`]): `succeeded`, `failed` or `interrupted`. The pass
//!   interrupts a turn that runs past [`RUN_DEADLINE_MS`]. An app server
//!   that stops fails the user's running runs, and so does a gateway
//!   restart ([`Routines::recover`]).
//!
//! Every change to a run goes out as [`Event::RoutineRunUpdated`] through
//! [`emit_run`], the one place a run's end is announced, so later work can
//! hang notifications on it. Three failed runs in a row pause the routine
//! ([`store::count_outcome`]).

pub mod schedule;
pub mod store;

use std::sync::Arc;
use std::time::Duration;

use bokkie_codex::protocol::TurnStatus;
use bokkie_protocol::api::{
    CreateRoutineRequest, Routine, RoutineRun, RunState, RunTrigger, UpdateRoutineRequest,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, RoutineId, Timestamp, UserId};
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
    emit_run(batch, &ended).await?;
    let failed = match state {
        RunState::Failed => Some(true),
        RunState::Interrupted if ended.deadline_passed_at.is_some() => Some(true),
        RunState::Succeeded => Some(false),
        _ => None,
    };
    if let Some(failed) = failed
        && let Some(routine) = store::count_outcome(batch, now, &ended, failed).await?
    {
        batch
            .emit(Event::RoutineUpdated {
                routine: routine.to_api(),
            })
            .await?;
    }
    Ok(Some(ended))
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
#[derive(Debug)]
pub struct Routines {
    db: SqlitePool,
    clock: Clock,
    events: Arc<EventLog>,
    conversations: Arc<Conversations>,
    /// Woken by "run now", so the scheduler starts the run at once.
    wake: tokio::sync::Notify,
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
        }
    }

    // -- The API ------------------------------------------------------------

    /// Creates a routine on one of the user's Bots, and announces it.
    pub async fn create(
        &self,
        user: UserId,
        bot: BotId,
        request: CreateRoutineRequest,
    ) -> Result<Routine> {
        let name = store::check_name(&request.name)?;
        let prompt = store::check_prompt(&request.prompt)?;
        schedule::check(&request.schedule)?;
        let now = self.clock.now();
        let mut batch = self.events.begin(user).await?;
        let account = own_account(&mut batch, user).await?;
        crate::bots::store::bot(&mut *batch, user, bot)
            .await?
            .ok_or_else(Failure::not_found)?;
        if store::count(&mut *batch, user).await? >= store::MAX_ROUTINES {
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
            &mut *batch,
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
        batch.commit().await?;
        tracing::info!(user = %user, bot = %bot, routine = %routine.id, "created a routine");
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
        let name = request.name.as_deref().map(store::check_name).transpose()?;
        let prompt = request
            .prompt
            .as_deref()
            .map(store::check_prompt)
            .transpose()?;
        if let Some(schedule) = &request.schedule {
            schedule::check(schedule)?;
        }
        let now = self.clock.now();
        let mut batch = self.events.begin(user).await?;
        let account = own_account(&mut batch, user).await?;
        let mut routine = store::routine(&mut *batch, user, bot, id)
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
        let routine = store::save(&mut *batch, now, &routine).await?.to_api();
        batch
            .emit(Event::RoutineUpdated {
                routine: routine.clone(),
            })
            .await?;
        batch.commit().await?;
        tracing::info!(user = %user, bot = %bot, routine = %id, paused = routine.paused, "updated a routine");
        Ok(routine)
    }

    /// Deletes one of the user's routines with its history, and announces
    /// it. A run of it that is running carries on as an ordinary turn.
    pub async fn delete(&self, user: UserId, bot: BotId, id: RoutineId) -> Result<()> {
        let mut batch = self.events.begin(user).await?;
        if !store::delete(&mut *batch, user, bot, id).await? {
            return Err(Failure::not_found());
        }
        batch
            .emit(Event::RoutineDeleted {
                bot_id: bot,
                routine_id: id,
            })
            .await?;
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
            self.try_start(&run, now).await?;
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
    /// starts waiting runs that can start, and interrupts runs past their
    /// deadline.
    pub async fn pass(&self, now: Timestamp) -> Result<()> {
        for routine in store::due(&self.db, now).await? {
            if let Err(failure) = self.fall_due(&routine, now).await {
                tracing::error!(user = %routine.user, routine = %routine.id, message = %failure.message, "queuing a routine's run failed");
            }
        }
        // Each waiting run starts in its own task, so that one user's slow
        // app server holds up nobody else's.
        let mut starts = tokio::task::JoinSet::new();
        for run in store::active_runs(&self.db, RunState::Waiting).await? {
            let routines = self.clone_parts();
            starts.spawn(async move {
                if let Err(failure) = routines.try_start(&run, now).await {
                    tracing::error!(user = %run.user, run = %run.id, message = %failure.message, "starting a routine's run failed");
                }
            });
        }
        while starts.join_next().await.is_some() {}
        for run in store::active_runs(&self.db, RunState::Running).await? {
            if let Err(failure) = self.enforce_deadline(&run, now).await {
                tracing::error!(user = %run.user, run = %run.id, message = %failure.message, "stopping a routine's run failed");
            }
        }
        Ok(())
    }

    /// A copy of the service for a task.
    fn clone_parts(&self) -> Self {
        Self::new(
            self.db.clone(),
            self.clock.clone(),
            self.events.clone(),
            self.conversations.clone(),
        )
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
        tracing::info!(user = %routine.user, routine = %routine.id, run = %run.id, state = store::state_name(state), "queued a routine's run");
        Ok(Some(run))
    }

    /// Tries to start a waiting run.
    async fn try_start(&self, run: &RunRow, now: Timestamp) -> Result<()> {
        let user = run.user;
        let Some(account) = accounts::account(&self.db, user).await? else {
            return Ok(());
        };
        if account.user.disabled {
            return self
                .end(run, now, RunState::Missed, Some(reasons::DISABLED))
                .await;
        }
        let Some(routine) = store::routine_of_user(&self.db, user, run.routine).await? else {
            return Ok(());
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
        match started {
            RoutineStart::Started { turn_id } => {
                tracing::info!(user = %user, run = %run.id, turn = %turn_id, "started a routine's run");
                Ok(())
            }
            RoutineStart::NotWaiting => Ok(()),
            RoutineStart::Busy => {
                if now.0 - run.queued_at.0 >= BUSY_WAIT_MS {
                    self.end(run, now, RunState::Skipped, Some(reasons::BUSY))
                        .await
                } else {
                    self.wait(run, Waiting::Busy, reasons::WAITING_BUSY).await
                }
            }
            RoutineStart::SignedOut => {
                if now.0 - run.scheduled_for.0 > MISFIRE_WINDOW_MS {
                    self.end(run, now, RunState::Missed, Some(reasons::SIGNED_OUT))
                        .await
                } else {
                    self.wait(run, Waiting::SignedOut, reasons::WAITING_SIGNED_OUT)
                        .await
                }
            }
            RoutineStart::Failed(reason) => {
                tracing::info!(user = %user, run = %run.id, %reason, "a routine's run failed to start");
                self.end(run, now, RunState::Failed, Some(&reason)).await
            }
        }
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

async fn own_account(batch: &mut Batch, user: UserId) -> Result<Account> {
    accounts::account(&mut **batch, user)
        .await?
        .ok_or_else(Failure::unauthenticated)
}
