//! Runs inside the Codex sandbox and reports what it can reach.
//!
//! ```text
//! bokkie-sandbox-probe check --spec-file <path> --nonce <nonce>
//! bokkie-sandbox-probe check --spec <json> --nonce <nonce>
//! bokkie-sandbox-probe linger <seconds>
//! ```
//!
//! `check` prints one JSON `ProbeReport` on standard output. `linger` starts a
//! `sleep <seconds>` in its own process group that ignores hang-up and
//! termination signals, then waits for it: the harness kills the sandbox's
//! parent and checks that the sleeper does not survive.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use bokkie_qualify::{ProbeSpec, run_probe};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("linger") => linger(&args[1..]),
        _ => {
            eprintln!("usage: bokkie-sandbox-probe check|linger ...");
            ExitCode::from(2)
        }
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn check(args: &[String]) -> ExitCode {
    let spec_json = match (option(args, "--spec"), option(args, "--spec-file")) {
        (Some(json), _) => json.to_owned(),
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(json) => json,
            Err(error) => {
                eprintln!("cannot read {path}: {error}");
                return ExitCode::from(2);
            }
        },
        (None, None) => {
            eprintln!("check needs --spec or --spec-file");
            return ExitCode::from(2);
        }
    };
    let spec: ProbeSpec = match serde_json::from_str(&spec_json) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("invalid spec: {error}");
            return ExitCode::from(2);
        }
    };
    let nonce = option(args, "--nonce").unwrap_or("probe");
    let report = run_probe(&spec, nonce);
    println!(
        "{}",
        serde_json::to_string(&report).expect("report serialises")
    );
    ExitCode::SUCCESS
}

fn linger(args: &[String]) -> ExitCode {
    let Some(seconds) = args.first().filter(|s| s.parse::<u64>().is_ok()) else {
        eprintln!("linger needs a whole number of seconds");
        return ExitCode::from(2);
    };
    // A detached sleeper in its own process group, ignoring the signals a
    // polite shutdown would send. Only a PID namespace teardown or SIGKILL
    // ends it.
    let sleeper = Command::new("/bin/sh")
        .args(["-c", "trap '' HUP INT TERM; exec sleep \"$0\""])
        .arg(seconds)
        .process_group(0)
        .spawn();
    match sleeper {
        Ok(mut child) => {
            println!("lingering {}", child.id());
            let _ = child.wait();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cannot start the sleeper: {error}");
            ExitCode::from(1)
        }
    }
}
