//! Isolated, manually clocked conversation qualification using the production router.
use bokkie::{
    DbExecutor, ManualClock, Store, UnixClock,
    conversation_http::ConversationConfig,
    conversation_runtime::ConversationProfile,
    http::{ApiState, router_with_state},
    http_security::ApiRuntime,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Component, PathBuf},
    sync::Arc,
};

const INITIAL_NOW: i64 = 1_790_028_000; // 22 September 2026, 07:30 Australia/Adelaide.
const MARKER: &str = "bokkie-conversation-synthetic-v1\n";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    now: Option<i64>,
    #[serde(default)]
    tick: bool,
    #[serde(default)]
    stop: bool,
    #[serde(default)]
    seed_calibration: bool,
}

// Fixed inactive synthetic data, available only on this marked fixture's stdin.
// The production HTTP router has no seeding operation.
fn seed_calibration(store: &mut Store, now: i64) -> Result<(), bokkie::StoreError> {
    if !store.managed_catalogue("", None, 1)?.items.is_empty()
        || store.conversation_model_dispatch_count()? != 0
    {
        return Err(bokkie::StoreError::Invalid(
            "calibration seed requires an empty unused synthetic fixture".into(),
        ));
    }
    for suffix in ["morning", "afternoon"] {
        let definition = bokkie::ManagedTaskDefinition::local_note(
            format!("Research queue reminder {suffix}"),
            "Review the synthetic research queue.",
        );
        store.managed_create(&format!("synthetic-calibration-{suffix}"), &definition, now)?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut profile_path = None;
    let mut root = None;
    let mut ui_dir = None;
    let mut resume = false;
    let mut preflight = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" => profile_path = args.next().map(PathBuf::from),
            "--root" => root = args.next().map(PathBuf::from),
            "--ui-dir" => ui_dir = args.next().map(PathBuf::from),
            "--resume" => resume = true,
            "--preflight" => preflight = true,
            _ => return Err(format!("unknown fixture option {arg}").into()),
        }
    }
    let profile = profile_path
        .map(|path| ConversationProfile::load(&path))
        .transpose()
        .map_err(io::Error::other)?
        .map(Arc::new);
    if preflight {
        let profile = profile.ok_or("--preflight requires --profile")?;
        println!(
            "{}",
            profile
                .preflight_tools(bokkie::conversation_tools::tools(false, false))
                .map_err(io::Error::other)?
        );
        return Ok(());
    }
    let root = root.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("bokkie-conversation-{}", uuid::Uuid::new_v4()))
    });
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("--root must be absolute without parent components".into());
    }
    let database = root.join("fixture.sqlite");
    let marker = root.join("SYNTHETIC_FIXTURE");
    let clock_file = root.join("clock.json");
    let now = if resume {
        if fs::canonicalize(&root)? != root
            || fs::symlink_metadata(&marker)?.file_type().is_symlink()
            || fs::metadata(&marker)?.len() != MARKER.len() as u64
            || fs::read_to_string(&marker)? != MARKER
            || !database.is_file()
            || fs::symlink_metadata(&database)?.file_type().is_symlink()
            || fs::symlink_metadata(&clock_file)?.file_type().is_symlink()
            || fs::metadata(&clock_file)?.len() > 32
        {
            return Err("--resume requires this fixture's synthetic root and database".into());
        }
        serde_json::from_slice::<i64>(&fs::read(&clock_file)?)?
    } else {
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&marker)?
            .write_all(MARKER.as_bytes())?;
        fs::write(&clock_file, INITIAL_NOW.to_string())?;
        INITIAL_NOW
    };
    if !(INITIAL_NOW..=INITIAL_NOW + 366 * 86400).contains(&now) {
        return Err("fixture clock exceeds its finite qualification window".into());
    }
    let clock = Arc::new(ManualClock::new(now));
    // Opening/migration belongs to fixture service startup, never an HTTP request.
    drop(Store::open(&database)?);
    let executor = DbExecutor::start(database)?;
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let address = listener.local_addr()?;
    let runtime = ApiRuntime::new(
        address,
        bokkie::migration_manifest().last().unwrap().version,
    )
    .map_err(|error| io::Error::other(format!("OS randomness unavailable: {error}")))?;
    let session = runtime.identity().session_id;
    executor
        .execute(move |store| store.conversation_interrupt(&session, now))
        .await?;
    let state = ApiState {
        executor: executor.clone(),
        runtime,
        engineering_intake: None,
        conversation: Some(ConversationConfig {
            profile,
            notes_enabled: true,
            clock: Some(clock.clone()),
        }),
    };
    println!(
        "{}",
        json!({"address":address,"root":root,"now":now,"resumed":resume,"database_kind":"synthetic_fixture","scheduler":"manual_stdin_only"})
    );
    io::stdout().flush()?;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(4);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if control_tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    let controls_executor = executor.clone();
    tokio::spawn(async move {
        while let Some(line) = control_rx.recv().await {
            let control = line.map_err(|error| error.to_string()).and_then(|line| {
                if line.len() > 4096 {
                    return Err("control exceeds byte bound".into());
                }
                serde_json::from_str::<Control>(&line).map_err(|error| error.to_string())
            });
            let control = match control {
                Ok(control) => control,
                Err(error) => {
                    println!("{}", json!({"error":error}));
                    continue;
                }
            };
            if control.seed_calibration && (control.now.is_some() || control.tick || control.stop) {
                println!(
                    "{}",
                    json!({"error":"seed_calibration cannot be combined with other controls"})
                );
                continue;
            }
            if control.stop {
                break;
            }
            if let Some(next) = control.now {
                if next < clock.now() || next > INITIAL_NOW + 366 * 86400 {
                    println!(
                        "{}",
                        json!({"error":"clock must advance within the finite qualification window"})
                    );
                    continue;
                }
                if let Err(error) = fs::write(&clock_file, next.to_string()) {
                    println!("{}", json!({"error":error.to_string()}));
                    continue;
                }
                clock.set(next);
            }
            let now = clock.now();
            let result = controls_executor
                .execute(move |store| {
                    if control.seed_calibration {
                        seed_calibration(store, now)?;
                    }
                    let ran = if control.tick {
                        bokkie::managed::run_one_note(store, now)?
                    } else {
                        false
                    };
                    let catalogue = store.managed_catalogue("", None, 100)?;
                    let mut details = Vec::new();
                    for entry in &catalogue.items {
                        if entry.kind == "managed" {
                            details.push(store.managed_detail(&entry.id)?);
                        }
                    }
                    Ok(json!({"now":now,"ran":ran,"catalogue":catalogue,"details":details,"model_calls":store.conversation_model_dispatch_count()?}))
                })
                .await;
            match result {
                Ok(receipt) => println!("{receipt}"),
                Err(error) => println!("{}", json!({"error":error.to_string()})),
            }
            let _ = io::stdout().flush();
        }
        let _ = stop_tx.send(());
    });
    axum::serve(listener, router_with_state(state, ui_dir))
        .with_graceful_shutdown(async {
            let _ = stop_rx.await;
        })
        .await?;
    executor.shutdown()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_seed_is_fixed_inactive_and_refuses_reuse() {
        let mut store = Store::open_in_memory().unwrap();
        seed_calibration(&mut store, INITIAL_NOW).unwrap();
        let catalogue = store
            .managed_catalogue("research queue", None, 100)
            .unwrap();
        assert_eq!(catalogue.items.len(), 2);
        for item in catalogue.items {
            let detail = store.managed_detail(&item.id).unwrap();
            assert_eq!(detail.status, bokkie::ManagedTaskStatus::Draft);
            assert!(detail.active.is_none());
            assert!(detail.runs.is_empty());
            assert!(detail.next_wake_at.is_none());
        }
        assert_eq!(store.conversation_model_dispatch_count().unwrap(), 0);
        assert!(seed_calibration(&mut store, INITIAL_NOW).is_err());
        assert_eq!(
            store.managed_catalogue("", None, 100).unwrap().items.len(),
            2
        );
    }
}
