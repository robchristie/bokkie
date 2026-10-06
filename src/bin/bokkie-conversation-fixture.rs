//! Isolated, manually clocked conversation qualification using the production router.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
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
    #[serde(default)]
    reminder_tick: bool,
    delivery: Option<SyntheticDelivery>,
    #[serde(default)]
    push_payload: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SyntheticDelivery {
    Accepted,
    RetryableRejection,
    PermanentRejection,
    SubscriptionExpired,
    Uncertain,
    CrashBeforeSend,
    CrashAfterDispatch,
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
    let mut port: u16 = 0;
    let mut resume = false;
    let mut preflight = false;
    let mut preflight_managed = false;
    let mut synthetic_reminders = false;
    let mut synthetic_push = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" => profile_path = args.next().map(PathBuf::from),
            "--root" => root = args.next().map(PathBuf::from),
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port requires a decimal port")?
                    .parse()?
            }
            "--ui-dir" => ui_dir = args.next().map(PathBuf::from),
            "--resume" => resume = true,
            "--synthetic-reminders" => synthetic_reminders = true,
            "--synthetic-push-reminders" => {
                synthetic_reminders = true;
                synthetic_push = true;
            }
            "--preflight" => preflight = true,
            "--preflight-managed" => {
                preflight = true;
                preflight_managed = true;
            }
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
                .preflight_tools(bokkie::conversation_tools::tools(preflight_managed, false,))
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
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
            .await?;
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
            push: synthetic_push.then(|| {
                Arc::new(bokkie::notifications::push::PushConfig {
                    vapid_private_key: URL_SAFE_NO_PAD.encode([7; 32]),
                    subject: "https://bokkie.example.org".into(),
                    timeout_ms: 100,
                    ttl_seconds: 3600,
                })
            }),
            notifications: (synthetic_reminders && !synthetic_push).then(|| {
                Arc::new(bokkie::notifications::NotificationConfig {
                    relay_host: "127.0.0.1".into(),
                    relay_port: 9,
                    from_address: "bokkie@example.invalid".into(),
                    destination: "fixture-recipient@example.invalid".into(),
                    timeout_ms: 100,
                })
            }),
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
            if control.seed_calibration
                && (control.now.is_some()
                    || control.tick
                    || control.stop
                    || control.reminder_tick
                    || control.delivery.is_some())
            {
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
                    if control.now.is_some() { store.recover_expired_leases(now)?; }
                    if control.seed_calibration {
                        seed_calibration(store, now)?;
                    }
                    let ran = if control.tick {
                        bokkie::managed::run_one_note(store, now)?
                    } else {
                        false
                    };
                    let reminder_ran = if control.reminder_tick && synthetic_reminders {
                        if let Some(claim) = store.claim_due_reminders(now, 30, 1)?.pop() {
                            let definition = store.managed_note_definition(&claim.obligation_id)?;
                            let result = bokkie::managed::render_local_note(&definition.definition);
                            store.complete_managed_note(&claim, &result, now)?;
                            true
                        } else { false }
                    } else { false };
                    if let Some(outcome) = control.delivery {
                        if !synthetic_reminders { return Err(bokkie::StoreError::Invalid("synthetic reminder controls are disabled".into())); }
                        if let Some(claim) = store.claim_due_notifications(now, 30, 1)?.pop() {
                            if !matches!(outcome, SyntheticDelivery::CrashBeforeSend) {
                                store.begin_notification_send(&claim, now)?;
                                use bokkie::notifications::NotificationOutcome;
                                let result = match outcome {
                                    SyntheticDelivery::Accepted => Some(if store.notification_intent(&claim.obligation_id)?.push.is_some(){NotificationOutcome::PushAccepted{detail:"Synthetic push service accepted; no external push was sent".into(),ttl_seconds:3600}}else{NotificationOutcome::Accepted { detail: "Synthetic relay accepted; no external email was sent".into() }}),
                                    SyntheticDelivery::RetryableRejection => Some(NotificationOutcome::Rejected { retryable: true, detail: "Synthetic temporary pre-acceptance rejection".into() }),
                                    SyntheticDelivery::SubscriptionExpired => Some(NotificationOutcome::SubscriptionExpired {detail:"Synthetic subscription expired; no external push was sent".into()}),
                                    SyntheticDelivery::PermanentRejection => Some(NotificationOutcome::Rejected { retryable: false, detail: "Synthetic permanent rejection".into() }),
                                    SyntheticDelivery::Uncertain => Some(NotificationOutcome::Uncertain { detail: "Synthetic acceptance acknowledgement was lost".into() }),
                                    SyntheticDelivery::CrashAfterDispatch | SyntheticDelivery::CrashBeforeSend => None,
                                };
                                if let Some(result) = result { store.complete_notification_send(&claim, result, now)?; }
                            }
                        }
                    }
                    let catalogue = store.managed_catalogue("", None, 100)?;
                    let mut details = Vec::new();
                    for entry in &catalogue.items {
                        if entry.kind == "managed" {
                            details.push(store.managed_detail(&entry.id)?);
                        }
                    }
                    let mut payload=None;
                    if control.push_payload {
                        if !synthetic_push{return Err(bokkie::StoreError::Invalid("Push payload output requires a marked synthetic push fixture".into()));}
                        for detail in &details {for run in &detail.runs {if let Some(delivery)=&run.delivery {let intent=store.notification_intent(&delivery.id)?;if intent.push.is_some(){payload=Some(serde_json::from_slice::<serde_json::Value>(&bokkie::notifications::push::payload(&intent).map_err(bokkie::StoreError::Invalid)?).map_err(|e|bokkie::StoreError::Invalid(e.to_string()))?);break;}}}}
                    }
                    Ok(json!({"push_payload":payload,"now":now,"ran":ran,"reminder_ran":reminder_ran,"catalogue":catalogue,"details":details,"model_calls":store.conversation_model_dispatch_count()?}))
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
