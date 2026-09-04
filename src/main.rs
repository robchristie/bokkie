use std::{
    io::{self, Read},
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
    ApprovalDecision, CANONICAL_DEFAULT_BRANCH, CANONICAL_REPOSITORY, GardenerRunnerError,
    GardenerRuntimeConfig, NewObligation, NewRepositoryRegistration, Recurrence, RetryPolicy,
    Store, StoreError, SystemClock, UnixClock,
    http::{error_json, router, router_with_ui, validate_loopback},
    runtime_trust::{ChildEnvironment, GitHubCredential},
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
// Clap requires the serve options inline; this short-lived CLI value is not
// cloned or retained, so boxing individual path arguments adds no useful bound.
#[allow(clippy::large_enum_variant)]
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
    /// Register, inspect, and decide coding-gardener state.
    Gardener {
        #[command(subcommand)]
        command: GardenerCommand,
    },
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
        /// Explicitly allow the scheduler to claim coding-gardener work.
        #[arg(long, requires = "gardener_worktree_root")]
        enable_coding_gardener: bool,
        /// Existing absolute parent directory for isolated gardener worktrees.
        #[arg(long, requires = "enable_coding_gardener")]
        gardener_worktree_root: Option<PathBuf>,
        #[arg(long, default_value = "/usr/bin/codex")]
        gardener_codex_executable: PathBuf,
        #[arg(long, default_value = "/usr/bin/git")]
        gardener_git_executable: PathBuf,
        #[arg(long, default_value = "/usr/bin/gh")]
        gardener_gh_executable: PathBuf,
        /// Absolute curl executable used only for credential-free public PR observation.
        #[arg(long, default_value = "/usr/bin/curl")]
        gardener_github_public_observer_executable: PathBuf,
        /// Absolute cargo executable used for fixed, credential-free candidate checks.
        #[arg(long, default_value = "/usr/bin/cargo")]
        gardener_cargo_executable: PathBuf,
        /// Absolute Bubblewrap executable used for Codex PID and candidate-check isolation.
        #[arg(long, default_value = "/usr/bin/bwrap")]
        gardener_candidate_sandbox_executable: PathBuf,
        /// Controlled HOME for Codex, Git, gh and candidate-check children.
        #[arg(long, requires = "enable_coding_gardener")]
        gardener_home: Option<PathBuf>,
        /// Read the optional GitHub mutation token once from standard input, then close it.
        #[arg(long, requires = "enable_coding_gardener")]
        gardener_github_token_stdin: bool,
        /// Narrative Codex profile identity retained in the run manifest when supplied.
        #[arg(long)]
        gardener_codex_profile: Option<String>,
        /// Narrative Codex model identity retained in the run manifest when supplied.
        #[arg(long)]
        gardener_codex_model: Option<String>,
        #[arg(long, default_value_t = 10_000)]
        gardener_heartbeat_ms: u64,
        /// Absolute wall-clock limit for each gardener child process.
        #[arg(long, default_value_t = 1_800_000)]
        gardener_process_timeout_ms: u64,
        /// Explicit static UI asset directory served at /ui on this loopback origin.
        #[arg(long)]
        ui_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum GardenerCommand {
    /// Register the canonical Bokkie checkout and inspection schedule.
    Register {
        #[arg(long, default_value = CANONICAL_REPOSITORY)]
        repository: String,
        #[arg(long, default_value = CANONICAL_DEFAULT_BRANCH)]
        default_branch: String,
        #[arg(long)]
        checkout_path: PathBuf,
        /// Unix timestamp; defaults to now.
        #[arg(long)]
        first_inspection_at: Option<i64>,
        #[arg(long, default_value = "0 0 * * *")]
        recurrence_cron: String,
        #[arg(long, default_value = "UTC")]
        recurrence_timezone: String,
    },
    /// Show the canonical repository registration.
    Repository,
    /// List or show persisted inspections.
    Inspections {
        #[command(subcommand)]
        command: GardenerInspectionCommand,
    },
    /// List, show, or decide immutable proposals.
    Proposals {
        #[command(subcommand)]
        command: GardenerProposalCommand,
    },
    /// List, show, or decide exact source-bound proposal instances.
    ProposalInstances {
        #[command(subcommand)]
        command: GardenerProposalInstanceCommand,
    },
    /// List or show implementation runs and their events.
    Runs {
        #[command(subcommand)]
        command: GardenerRunCommand,
    },
}

#[derive(Debug, Subcommand)]
enum GardenerInspectionCommand {
    List,
    Show { id: String },
}

#[derive(Debug, Subcommand)]
enum GardenerProposalCommand {
    List,
    Show {
        fingerprint: String,
    },
    Observations {
        fingerprint: String,
    },
    Approve {
        fingerprint: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        note: Option<String>,
    },
    Reject {
        fingerprint: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum GardenerProposalInstanceCommand {
    List,
    Show {
        instance_id: String,
    },
    Observations {
        instance_id: String,
    },
    Approve {
        instance_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        note: Option<String>,
    },
    Reject {
        instance_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum GardenerRunCommand {
    List,
    Show { id: String },
    Events { id: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FakeOutcomeArg {
    Succeed,
    FailRetryable,
    FailTerminal,
}

struct ServeOptions {
    bind: SocketAddr,
    poll_ms: u64,
    lease_seconds: i64,
    fake_delay_ms: u64,
    fake_outcome: ServiceFakeOutcome,
    enable_coding_gardener: bool,
    gardener_worktree_root: Option<PathBuf>,
    gardener_codex_executable: PathBuf,
    gardener_git_executable: PathBuf,
    gardener_gh_executable: PathBuf,
    gardener_github_public_observer_executable: PathBuf,
    gardener_cargo_executable: PathBuf,
    gardener_candidate_sandbox_executable: PathBuf,
    gardener_home: Option<PathBuf>,
    gardener_github_token_stdin: bool,
    gardener_codex_profile: Option<String>,
    gardener_codex_model: Option<String>,
    gardener_heartbeat_ms: u64,
    gardener_process_timeout_ms: u64,
    ui_dir: Option<PathBuf>,
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
            | Self::Scheduler(SchedulerError::Gardener(GardenerRunnerError::Configuration(_)))
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
            | Self::Scheduler(SchedulerError::Gardener(GardenerRunnerError::Configuration(_)))
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
            enable_coding_gardener,
            gardener_worktree_root,
            gardener_codex_executable,
            gardener_git_executable,
            gardener_gh_executable,
            gardener_github_public_observer_executable,
            gardener_cargo_executable,
            gardener_candidate_sandbox_executable,
            gardener_home,
            gardener_github_token_stdin,
            gardener_codex_profile,
            gardener_codex_model,
            gardener_heartbeat_ms,
            gardener_process_timeout_ms,
            ui_dir,
        } => {
            serve(
                cli.database,
                ServeOptions {
                    bind,
                    poll_ms,
                    lease_seconds,
                    fake_delay_ms,
                    fake_outcome: fake_outcome.into(),
                    enable_coding_gardener,
                    gardener_worktree_root,
                    gardener_codex_executable,
                    gardener_git_executable,
                    gardener_gh_executable,
                    gardener_github_public_observer_executable,
                    gardener_cargo_executable,
                    gardener_candidate_sandbox_executable,
                    gardener_home,
                    gardener_github_token_stdin,
                    gardener_codex_profile,
                    gardener_codex_model,
                    gardener_heartbeat_ms,
                    gardener_process_timeout_ms,
                    ui_dir,
                },
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
        Command::Gardener { command } => run_gardener_command(&mut store, command, now)?,
        Command::Serve { .. } => unreachable!("serve was handled asynchronously"),
    }
    Ok(())
}

fn run_gardener_command(
    store: &mut Store,
    command: GardenerCommand,
    now: i64,
) -> Result<(), StoreError> {
    match command {
        GardenerCommand::Register {
            repository,
            default_branch,
            checkout_path,
            first_inspection_at,
            recurrence_cron,
            recurrence_timezone,
        } => print_json(&store.register_gardener_repository(
            NewRepositoryRegistration {
                repository,
                default_branch,
                checkout_path: checkout_path.to_string_lossy().into_owned(),
                inspection_recurrence: Recurrence::new(recurrence_cron, recurrence_timezone)?,
                first_inspection_at: first_inspection_at.unwrap_or(now),
            },
            now,
        )?),
        GardenerCommand::Repository => print_json(&require_gardener_repository(store)?),
        GardenerCommand::Inspections { command } => match command {
            GardenerInspectionCommand::List => print_json(&store.gardener_inspections()?),
            GardenerInspectionCommand::Show { id } => {
                print_json(&require_gardener_inspection(store, &id)?)
            }
        },
        GardenerCommand::Proposals { command } => match command {
            GardenerProposalCommand::List => print_json(&store.gardener_proposals()?),
            GardenerProposalCommand::Show { fingerprint } => {
                print_json(&require_gardener_proposal(store, &fingerprint)?)
            }
            GardenerProposalCommand::Observations { fingerprint } => {
                require_gardener_proposal(store, &fingerprint)?;
                print_json(&store.proposal_observations(&fingerprint)?);
            }
            GardenerProposalCommand::Approve {
                fingerprint,
                actor,
                note,
            } => print_json(&store.decide_gardener_proposal(
                &fingerprint,
                ApprovalDecision::Approved,
                &actor,
                note.as_deref(),
                now,
            )?),
            GardenerProposalCommand::Reject {
                fingerprint,
                actor,
                note,
            } => print_json(&store.decide_gardener_proposal(
                &fingerprint,
                ApprovalDecision::Rejected,
                &actor,
                note.as_deref(),
                now,
            )?),
        },
        GardenerCommand::ProposalInstances { command } => match command {
            GardenerProposalInstanceCommand::List => {
                print_json(&all_gardener_proposal_instances(store)?)
            }
            GardenerProposalInstanceCommand::Show { instance_id } => {
                print_json(&require_gardener_proposal_instance(store, &instance_id)?)
            }
            GardenerProposalInstanceCommand::Observations { instance_id } => {
                require_gardener_proposal_instance(store, &instance_id)?;
                print_json(&store.proposal_instance_observations(&instance_id)?);
            }
            GardenerProposalInstanceCommand::Approve {
                instance_id,
                actor,
                note,
            } => print_json(&store.decide_gardener_proposal_instance(
                &instance_id,
                ApprovalDecision::Approved,
                &actor,
                note.as_deref(),
                now,
            )?),
            GardenerProposalInstanceCommand::Reject {
                instance_id,
                actor,
                note,
            } => print_json(&store.decide_gardener_proposal_instance(
                &instance_id,
                ApprovalDecision::Rejected,
                &actor,
                note.as_deref(),
                now,
            )?),
        },
        GardenerCommand::Runs { command } => match command {
            GardenerRunCommand::List => print_json(&store.gardener_implementation_runs()?),
            GardenerRunCommand::Show { id } => print_json(&require_gardener_run(store, &id)?),
            GardenerRunCommand::Events { id } => {
                require_gardener_run(store, &id)?;
                print_json(&store.gardener_run_events(&id)?);
            }
        },
    }
    Ok(())
}

fn require_gardener_repository(
    store: &Store,
) -> Result<bokkie::RepositoryRegistration, StoreError> {
    store
        .gardener_repository()?
        .ok_or_else(|| StoreError::NotFound(CANONICAL_REPOSITORY.to_owned()))
}

fn require_gardener_inspection(
    store: &Store,
    id: &str,
) -> Result<bokkie::GardenerInspection, StoreError> {
    store
        .gardener_inspection(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

fn require_gardener_proposal(
    store: &Store,
    fingerprint: &str,
) -> Result<bokkie::Proposal, StoreError> {
    store
        .gardener_proposal(fingerprint)?
        .ok_or_else(|| StoreError::NotFound(fingerprint.to_owned()))
}

fn require_gardener_proposal_instance(
    store: &Store,
    instance_id: &str,
) -> Result<bokkie::gardener::ProposalInstance, StoreError> {
    store
        .gardener_proposal_instance(instance_id)?
        .ok_or_else(|| StoreError::NotFound(instance_id.to_owned()))
}

fn all_gardener_proposal_instances(
    store: &Store,
) -> Result<Vec<bokkie::gardener::ProposalInstance>, StoreError> {
    let mut instances = Vec::new();
    for proposal in store.gardener_proposals()? {
        instances.extend(store.gardener_proposal_instances(&proposal.fingerprint)?);
    }
    Ok(instances)
}

fn require_gardener_run(
    store: &Store,
    id: &str,
) -> Result<bokkie::GardenerImplementationRun, StoreError> {
    store
        .gardener_implementation_run(id)?
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
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

async fn serve(database: PathBuf, options: ServeOptions) -> Result<(), AppError> {
    validate_loopback(options.bind).map_err(AppError::Configuration)?;
    if options.poll_ms == 0 {
        return Err(AppError::Configuration(
            "poll-ms must be positive".to_owned(),
        ));
    }
    if options.lease_seconds < 2 {
        return Err(AppError::Configuration(
            "lease-seconds must be at least 2 for second-resolution lease renewal".to_owned(),
        ));
    }
    if let Some(ui_dir) = &options.ui_dir {
        if !ui_dir.is_dir() {
            return Err(AppError::Configuration(format!(
                "ui-dir must be an existing directory: {}",
                ui_dir.display()
            )));
        }
    }

    // Bind before starting the scheduler so a port conflict cannot execute work in a
    // process that immediately fails service start-up.
    let listener = tokio::net::TcpListener::bind(options.bind).await?;
    let local_address = listener.local_addr()?;
    let scheduler_config = SchedulerConfig {
        database: database.clone(),
        poll_interval: Duration::from_millis(options.poll_ms),
        lease_seconds: options.lease_seconds,
        fake_delay: Duration::from_millis(options.fake_delay_ms),
        fake_outcome: options.fake_outcome,
    };
    let mut scheduler = if options.enable_coding_gardener {
        let worktree_root = options.gardener_worktree_root.ok_or_else(|| {
            AppError::Configuration(
                "gardener-worktree-root is required when coding gardener is enabled".to_owned(),
            )
        })?;
        let gardener_home = options.gardener_home.unwrap_or_else(|| {
            worktree_root
                .parent()
                .unwrap_or(&worktree_root)
                .to_path_buf()
        });
        if !gardener_home.is_absolute() || !gardener_home.is_dir() {
            return Err(AppError::Configuration(format!(
                "gardener-home must be an existing absolute directory: {}",
                gardener_home.display()
            )));
        }
        let child_path = controlled_executable_path(&[
            &options.gardener_codex_executable,
            &options.gardener_git_executable,
            &options.gardener_gh_executable,
            &options.gardener_github_public_observer_executable,
            &options.gardener_cargo_executable,
            &options.gardener_candidate_sandbox_executable,
        ])?;
        let environment = ChildEnvironment::new(
            &gardener_home,
            gardener_home.join(".config/bokkie-gardener"),
            gardener_home.join(".cache/bokkie-gardener"),
            child_path,
        )
        .map_err(|error| AppError::Configuration(error.to_string()))?;
        let credential = options
            .gardener_github_token_stdin
            .then(read_github_credential_from_stdin)
            .transpose()?;
        if credential.is_some() {
            protect_process_credentials()?;
        }
        let mut gardener = GardenerRuntimeConfig::new(
            worktree_root,
            options.gardener_codex_executable,
            options.gardener_git_executable,
            options.gardener_gh_executable,
        )
        .with_child_environment(environment)
        .with_github_public_observer(options.gardener_github_public_observer_executable)
        .with_candidate_sandbox(options.gardener_candidate_sandbox_executable)
        .with_candidate_checks(
            options.gardener_cargo_executable,
            [
                vec!["test", "--all-targets", "--locked"],
                vec![
                    "clippy",
                    "--all-targets",
                    "--all-features",
                    "--locked",
                    "--",
                    "-D",
                    "warnings",
                ],
                vec!["fmt", "--all", "--", "--check"],
            ],
        )
        .with_codex_identity(options.gardener_codex_profile, options.gardener_codex_model)
        .with_heartbeat_interval(Duration::from_millis(options.gardener_heartbeat_ms))
        .with_process_timeout(Duration::from_millis(options.gardener_process_timeout_ms));
        if let Some(credential) = credential {
            gardener = gardener.with_github_credential(credential);
        }
        Scheduler::start_configured(scheduler_config, Some(gardener))?
    } else {
        Scheduler::start(scheduler_config)?
    };
    let stop = scheduler.stop_flag();
    let scheduler_exit = scheduler.take_exit_signal();
    eprintln!(
        "{}",
        json!({"event": "listening", "address": local_address.to_string()})
    );

    let application = match options.ui_dir {
        Some(ui_dir) => router_with_ui(database, ui_dir),
        None => router(database),
    };
    let server_result = axum::serve(listener, application)
        .with_graceful_shutdown(shutdown_signal(stop, scheduler_exit))
        .await;
    let scheduler_result = scheduler.shutdown();
    server_result?;
    scheduler_result?;
    eprintln!("{}", json!({"event": "stopped"}));
    Ok(())
}

fn controlled_executable_path(paths: &[&PathBuf]) -> Result<Vec<PathBuf>, AppError> {
    let mut directories = Vec::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(AppError::Configuration(format!(
                "gardener executable paths must be absolute: {}",
                path.display()
            )));
        }
        let parent = path.parent().ok_or_else(|| {
            AppError::Configuration(format!(
                "gardener executable has no parent: {}",
                path.display()
            ))
        })?;
        if !directories.iter().any(|existing| existing == parent) {
            directories.push(parent.to_path_buf());
        }
    }
    Ok(directories)
}

fn read_github_credential_from_stdin() -> Result<GitHubCredential, AppError> {
    let mut token = Vec::new();
    let read_result = io::stdin().take(16 * 1024 + 1).read_to_end(&mut token);
    // SAFETY: serve mode has explicitly consumed standard input as a one-shot
    // credential channel. Closing the descriptor before any child starts is
    // the security boundary; every supervised child receives its own pipe.
    let close_result = unsafe { libc::close(libc::STDIN_FILENO) };
    read_result.map_err(AppError::Io)?;
    if close_result != 0 {
        return Err(AppError::Io(io::Error::last_os_error()));
    }
    if token.len() > 16 * 1024 {
        return Err(AppError::Configuration(
            "gardener GitHub token input must be no larger than 16384 bytes".to_owned(),
        ));
    }
    let token = String::from_utf8(token).map_err(|_| {
        AppError::Configuration("gardener GitHub token input must be valid UTF-8".to_owned())
    })?;
    GitHubCredential::new(token.trim()).map_err(|error| AppError::Configuration(error.to_string()))
}

fn protect_process_credentials() -> Result<(), AppError> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: PR_SET_DUMPABLE changes only the current process attribute.
        // It prevents same-UID children from attaching to or opening sensitive
        // `/proc` state belonging to the credential-holding daemon.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(AppError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(AppError::Configuration(
            "gardener GitHub token input requires Linux process protection".to_owned(),
        ))
    }
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
    use std::{
        fs::File,
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn one_shot_credential_input_is_closed_and_unavailable_to_a_child() {
        const HELPER: &str = "BOKKIE_CREDENTIAL_BOUNDARY_HELPER";
        if std::env::var_os(HELPER).is_some() {
            let credential = read_github_credential_from_stdin().unwrap();
            protect_process_credentials().unwrap();
            assert_eq!(format!("{credential:?}"), "GitHubCredential([REDACTED])");
            let sentinel = std::env::var_os("BOKKIE_CREDENTIAL_SENTINEL").unwrap();
            let parent = std::process::id().to_string();
            let status = Command::new("/bin/sh")
                .args([
                    "-c",
                    "[ ! -r \"$1\" ] || exit 21; [ ! -e \"/proc/$2/fd/0\" ] || exit 22; [ -z \"${GH_TOKEN-}${GITHUB_TOKEN-}${AWS_SECRET_ACCESS_KEY-}${SSH_AUTH_SOCK-}\" ] || exit 23",
                    "credential-boundary-probe",
                ])
                .arg(sentinel)
                .arg(parent)
                .env_clear()
                .stdin(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let sentinel = root.path().join("github-token");
        std::fs::write(&sentinel, "test-token\n").unwrap();
        let input = File::open(&sentinel).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::one_shot_credential_input_is_closed_and_unavailable_to_a_child",
                "--nocapture",
            ])
            .env(HELPER, "1")
            .env("BOKKIE_CREDENTIAL_SENTINEL", &sentinel)
            .env("GH_TOKEN", "ambient-secret")
            .stdin(input)
            .status()
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(status.success());
    }
}
