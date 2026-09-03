use std::{
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bokkie::{
    ApprovalDecision, NewObligation, Recurrence, RetryPolicy, Store, StoreError, SystemClock,
    UnixClock,
    http::{error_json, router, validate_loopback},
    service::{Scheduler, SchedulerConfig, SchedulerError, ServiceFakeOutcome},
};
#[cfg(test)]
use clap::CommandFactory;
use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "bokkie",
    version,
    about = "Durable local-first obligation kernel"
)]
struct Cli {
    /// SQLite database path.
    #[arg(
        long,
        global = true,
        env = "BOKKIE_DATABASE",
        default_value = "bokkie.sqlite"
    )]
    database: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an obligation.
    Create {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        description: String,
        /// Unix timestamp; defaults to now.
        #[arg(long)]
        scheduled_at: Option<i64>,
        #[arg(long, requires = "recurrence_timezone")]
        recurrence_cron: Option<String>,
        #[arg(long, requires = "recurrence_cron")]
        recurrence_timezone: Option<String>,
        #[arg(long)]
        approval_required: bool,
        #[arg(long, default_value_t = 3)]
        max_attempts: u32,
        #[arg(long, default_value_t = 30)]
        retry_base_seconds: i64,
        #[arg(long, default_value_t = 3_600)]
        retry_max_seconds: i64,
    },
    /// List all obligations.
    List,
    /// Show one obligation.
    Show { id: String },
    /// Approve the current occurrence.
    Approve {
        id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Reject the current occurrence.
    Reject {
        id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Retry an obligation requiring human attention.
    Retry { id: String },
    /// Cancel a non-terminal, non-running obligation.
    Cancel { id: String },
    /// List append-only audit events for an obligation.
    Events { id: String },
    /// List execution attempts for an obligation.
    Attempts { id: String },
    /// Run the loopback API and background scheduler.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7744")]
        bind: SocketAddr,
        #[arg(long, default_value_t = 100)]
        poll_ms: u64,
        #[arg(long, default_value_t = 30)]
        lease_seconds: i64,
        #[arg(long, default_value_t = 0)]
        fake_delay_ms: u64,
        #[arg(long, value_enum, default_value_t = FakeOutcomeArg::Succeed)]
        fake_outcome: FakeOutcomeArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FakeOutcomeArg {
    Succeed,
    FailRetryable,
    FailTerminal,
}

impl From<FakeOutcomeArg> for ServiceFakeOutcome {
    fn from(value: FakeOutcomeArg) -> Self {
        match value {
            FakeOutcomeArg::Succeed => Self::Succeed,
            FakeOutcomeArg::FailRetryable => Self::FailRetryable,
            FakeOutcomeArg::FailTerminal => Self::FailTerminal,
        }
    }
}

#[derive(Debug, Error)]
enum AppError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
    #[error("invalid configuration: {0}")]
    Configuration(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl AppError {
    fn code(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound(_)) => "not_found",
            Self::Store(StoreError::Invalid(_) | StoreError::Recurrence(_))
            | Self::Configuration(_) => "invalid_request",
            Self::Store(StoreError::Conflict(_) | StoreError::Fenced) => "transition_conflict",
            Self::Store(StoreError::Sql(_)) => "storage_error",
            Self::Scheduler(_) => "scheduler_error",
            Self::Io(_) => "service_error",
        }
    }

    fn exit_code(&self) -> u8 {
        match self {
            Self::Store(StoreError::NotFound(_)) => 3,
            Self::Store(StoreError::Conflict(_) | StoreError::Fenced) => 4,
            Self::Store(StoreError::Invalid(_) | StoreError::Recurrence(_))
            | Self::Configuration(_) => 2,
            _ => 1,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let message = error.to_string();
            eprintln!("{}", error_json("invalid_arguments", message));
            return ExitCode::from(2);
        }
    };

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error_json(error.code(), error.to_string()));
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<(), AppError> {
    match cli.command {
        Command::Serve {
            bind,
            poll_ms,
            lease_seconds,
            fake_delay_ms,
            fake_outcome,
        } => {
            serve(
                cli.database,
                bind,
                poll_ms,
                lease_seconds,
                fake_delay_ms,
                fake_outcome.into(),
            )
            .await
        }
        command => run_store_command(cli.database, command),
    }
}

fn run_store_command(database: PathBuf, command: Command) -> Result<(), AppError> {
    let mut store = Store::open(database)?;
    let now = SystemClock.now();
    match command {
        Command::Create {
            id,
            description,
            scheduled_at,
            recurrence_cron,
            recurrence_timezone,
            approval_required,
            max_attempts,
            retry_base_seconds,
            retry_max_seconds,
        } => {
            let recurrence = match (recurrence_cron, recurrence_timezone) {
                (Some(expression), Some(timezone)) => {
                    Some(Recurrence::new(expression, timezone).map_err(StoreError::from)?)
                }
                (None, None) => None,
                _ => unreachable!("clap requires recurrence arguments together"),
            };
            print_json(&store.create(
                NewObligation {
                    id: id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                    description,
                    scheduled_at: scheduled_at.unwrap_or(now),
                    recurrence,
                    approval_required,
                    retry: RetryPolicy {
                        max_attempts,
                        base_delay_seconds: retry_base_seconds,
                        max_delay_seconds: retry_max_seconds,
                    },
                },
                now,
            )?);
        }
        Command::List => print_json(&store.list()?),
        Command::Show { id } => print_json(&require_obligation(&store, &id)?),
        Command::Approve { id, actor, note } => {
            decide_and_print(
                &mut store,
                &id,
                ApprovalDecision::Approved,
                &actor,
                note.as_deref(),
                now,
            )?;
        }
        Command::Reject { id, actor, note } => {
            decide_and_print(
                &mut store,
                &id,
                ApprovalDecision::Rejected,
                &actor,
                note.as_deref(),
                now,
            )?;
        }
        Command::Retry { id } => {
            store.retry_attention(&id, now)?;
            print_json(&require_obligation(&store, &id)?);
        }
        Command::Cancel { id } => {
            store.cancel(&id, now)?;
            print_json(&require_obligation(&store, &id)?);
        }
        Command::Events { id } => {
            require_obligation(&store, &id)?;
            print_json(&store.events(&id)?);
        }
        Command::Attempts { id } => {
            require_obligation(&store, &id)?;
            print_json(&store.attempts(&id)?);
        }
        Command::Serve { .. } => unreachable!("serve was handled asynchronously"),
    }
    Ok(())
}

fn decide_and_print(
    store: &mut Store,
    id: &str,
    decision: ApprovalDecision,
    actor: &str,
    note: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    store.decide_approval(id, decision, actor, note, now)?;
    print_json(&require_obligation(store, id)?);
    Ok(())
}

fn require_obligation(store: &Store, id: &str) -> Result<bokkie::Obligation, StoreError> {
    store
        .get(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

async fn serve(
    database: PathBuf,
    bind: SocketAddr,
    poll_ms: u64,
    lease_seconds: i64,
    fake_delay_ms: u64,
    fake_outcome: ServiceFakeOutcome,
) -> Result<(), AppError> {
    validate_loopback(bind).map_err(AppError::Configuration)?;
    if poll_ms == 0 {
        return Err(AppError::Configuration(
            "poll-ms must be positive".to_owned(),
        ));
    }
    if lease_seconds < 2 {
        return Err(AppError::Configuration(
            "lease-seconds must be at least 2 for second-resolution lease renewal".to_owned(),
        ));
    }

    // Bind before starting the scheduler so a port conflict cannot execute work in a
    // process that immediately fails service start-up.
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local_address = listener.local_addr()?;
    let mut scheduler = Scheduler::start(SchedulerConfig {
        database: database.clone(),
        poll_interval: Duration::from_millis(poll_ms),
        lease_seconds,
        fake_delay: Duration::from_millis(fake_delay_ms),
        fake_outcome,
    })?;
    let stop = scheduler.stop_flag();
    let scheduler_exit = scheduler.take_exit_signal();
    eprintln!(
        "{}",
        json!({"event": "listening", "address": local_address.to_string()})
    );

    let server_result = axum::serve(listener, router(database))
        .with_graceful_shutdown(shutdown_signal(stop, scheduler_exit))
        .await;
    let scheduler_result = scheduler.shutdown();
    server_result?;
    scheduler_result?;
    eprintln!("{}", json!({"event": "stopped"}));
    Ok(())
}

async fn shutdown_signal(
    stop: Arc<AtomicBool>,
    scheduler_exit: tokio::sync::oneshot::Receiver<()>,
) {
    tokio::select! {
        () = operating_system_shutdown_signal() => {}
        _ = scheduler_exit => {}
    }

    // This is set as soon as either shutdown source resolves, before HTTP draining
    // and scheduler joining.
    stop.store(true, Ordering::SeqCst);
}

async fn operating_system_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.expect("failed to install Ctrl-C handler");
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl-C handler");
}

fn print_json(value: &impl Serialize) {
    println!(
        "{}",
        serde_json::to_string(value).expect("serialising a public response cannot fail")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
