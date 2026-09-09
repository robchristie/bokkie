//! Explicit local-only engineering runtime entrypoint; no daemon installation.
use bokkie::{Store, engineering_runtime};
use clap::{Parser, Subcommand};
use engineering_runtime::{EngineeringRuntime, EngineeringRuntimeProfile, RuntimeResult};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Bounded local Codex supervision (fixture qualification required before dogfood)"
)]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    profile: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Save ordinary intent and a bootstrap contract. No model execution.
    Intake {
        #[arg(long)]
        intent: String,
        #[arg(long)]
        command_id: String,
    },
    /// Validate the explicit task profile without starting Codex.
    Validate,
    /// Drive one reconciliation/dispatch iteration. Explicitly uses Codex account.
    Tick,
    /// Run bounded iterations until interrupted; brokers survive this controller.
    Run {
        #[arg(long, default_value_t = 5)]
        poll_seconds: u64,
    },
}
fn main() -> RuntimeResult<()> {
    let cli = Cli::parse();
    let profile = EngineeringRuntimeProfile::load(&cli.profile)?;
    if matches!(cli.command, Action::Validate) {
        println!("{}", serde_json::to_string_pretty(&profile)?);
        return Ok(());
    }
    profile.validate_database(&cli.db)?;
    let mut store = Store::open(&cli.db)?;
    if let Action::Intake { intent, command_id } = cli.command {
        let receipt = engineering_runtime::intake(
            &mut store,
            &profile,
            intent,
            command_id,
            chrono::Utc::now().timestamp(),
        )?;
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(());
    }
    let runtime = EngineeringRuntime::new(profile)?;
    match cli.command {
        Action::Tick => println!(
            "{}",
            runtime.tick(&mut store, chrono::Utc::now().timestamp())?
        ),
        Action::Run { poll_seconds } => {
            if !(1..=30).contains(&poll_seconds) {
                return Err("poll interval must be 1–30 seconds".into());
            }
            loop {
                println!(
                    "{}",
                    runtime.tick(&mut store, chrono::Utc::now().timestamp())?
                );
                std::thread::sleep(std::time::Duration::from_secs(poll_seconds));
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
