//! Side-effect-free qualification service for the Bokkie operator UI.
//!
//! This executable intentionally has no database-path or runner options. It
//! creates one new database beneath its own temporary root, binds only a
//! literal loopback address, and serves the production router and supplied UI.

use std::{
    error::Error,
    fs,
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use bokkie::{
    ApprovalDecision, Completion, DbExecutor, GardenerCandidateQualification,
    GardenerVerificationVerdict, InspectionResult, NewGardenerImplementationRun,
    NewGardenerInspection, NewObligation, NewRepositoryRegistration, Recurrence, RetryPolicy,
    Store, http::router_with_ui_executor, http_security::ApiRuntime,
};
use uuid::Uuid;

const NOW: i64 = 1_788_381_000;
const FUTURE: i64 = NOW + 315_360_000;

struct FixtureRoot(PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut ui_dir = None;
    let mut variant = "full".to_owned();
    let mut port = 0_u16;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--ui-dir" => ui_dir = arguments.next().map(PathBuf::from),
            "--variant" => variant = arguments.next().ok_or("--variant needs a value")?,
            "--port" => port = arguments.next().ok_or("--port needs a value")?.parse()?,
            _ => return Err(format!("unsupported fixture argument {argument:?}").into()),
        }
    }
    let ui_dir = ui_dir.ok_or("--ui-dir is required")?;
    if !ui_dir.is_dir() {
        return Err(format!("UI directory {} does not exist", ui_dir.display()).into());
    }
    if !matches!(variant.as_str(), "full" | "empty" | "empty-inbox" | "large") {
        return Err("--variant must be full, empty, empty-inbox, or large".into());
    }

    let root = FixtureRoot(
        std::env::temp_dir().join(format!("bokkie-ui-qualification-{}", Uuid::new_v4())),
    );
    fs::create_dir(&root.0)?;
    let database = root.0.join("fixture.sqlite");
    let mut store = Store::open(&database)?;
    match variant.as_str() {
        "full" => seed_full(&mut store, &root.0)?,
        "empty-inbox" => seed_empty_inbox(&mut store)?,
        "large" => seed_large(&mut store)?,
        "empty" => {}
        _ => unreachable!(),
    }
    drop(store);
    let database_executor = DbExecutor::start(database)?;

    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
            .await?;
    let address = listener.local_addr()?;
    let runtime = ApiRuntime::new(
        address,
        bokkie::migration_manifest().last().unwrap().version,
    )
    .map_err(|error| io::Error::other(format!("OS randomness unavailable: {error}")))?;
    let service_identity = runtime.identity();
    println!(
        "{}",
        serde_json::json!({
            "address": address,
            "variant": variant,
            "database_kind": "new_temporary_fixture",
            "gardener_runtime": false,
            "root": root.0,
            "service": service_identity,
        })
    );
    io::stdout().flush()?;
    axum::serve(
        listener,
        router_with_ui_executor(database_executor.clone(), ui_dir, runtime),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    database_executor.shutdown()?;
    Ok(())
}

fn obligation(id: &str, description: String, scheduled_at: i64, approval: bool) -> NewObligation {
    NewObligation {
        id: id.to_owned(),
        description,
        scheduled_at,
        recurrence: None,
        approval_required: approval,
        retry: RetryPolicy::default(),
    }
}

fn seed_full(store: &mut Store, root: &Path) -> Result<(), Box<dyn Error>> {
    store.create(
        obligation(
            "pending-long-identity-00000000000000000000000000000001",
            "Pending scheduled work with a deliberately long description that proves wrapping without hiding the durable wake-up or operator identity".to_owned(),
            FUTURE,
            false,
        ),
        NOW,
    )?;
    store.create(
        obligation(
            "approval-safe-cancel",
            "Safe temporary approval-bound lifecycle action".to_owned(),
            FUTURE,
            true,
        ),
        NOW,
    )?;
    store.create(
        obligation(
            "running-active-lease",
            "Active leased work retained only for geometry and liveness observation".to_owned(),
            NOW,
            false,
        ),
        NOW - 10,
    )?;
    let running = store.claim_due(NOW, 315_360_000, 1)?.remove(0);
    assert_eq!(running.obligation_id, "running-active-lease");

    seed_failure(store, "retry-scheduled", true, 3)?;
    seed_failure(store, "attention-nonretryable", false, 3)?;
    seed_failure(store, "attention-exhausted", true, 1)?;
    store.create(
        obligation(
            "generic-rejected",
            "Rejected generic approval with exact actor and note provenance".to_owned(),
            FUTURE,
            true,
        ),
        NOW,
    )?;
    store.decide_approval(
        "generic-rejected",
        ApprovalDecision::Rejected,
        "qualification-operator",
        Some("Deliberately rejected fixture decision"),
        NOW + 1,
    )?;

    store.create(
        obligation(
            "completed-with-long-evidence",
            "Completed representative work".to_owned(),
            NOW,
            false,
        ),
        NOW - 10,
    )?;
    let claim = store.claim_due(NOW, 60, 1)?.remove(0);
    store.complete(
        &claim,
        Completion::Succeeded {
            evidence: Some(format!(
                "source={} input={} result={} retained evidence remains selectable",
                "c".repeat(40),
                "fixture-input-".to_owned() + &"x".repeat(80),
                "pass"
            )),
        },
        NOW + 1,
    )?;
    store.create(
        obligation(
            "cancelled-representative",
            "Cancelled representative work".to_owned(),
            FUTURE,
            false,
        ),
        NOW,
    )?;
    store.cancel("cancelled-representative", NOW + 1)?;

    seed_gardener_states(store, root)?;
    for index in 0..180 {
        store.create(
            obligation(
                &format!("ledger-{index:04}"),
                format!("Representative ledger item {index:04}"),
                FUTURE + i64::from(index),
                false,
            ),
            NOW,
        )?;
    }
    Ok(())
}

fn seed_failure(
    store: &mut Store,
    id: &str,
    retryable: bool,
    max_attempts: u32,
) -> Result<(), Box<dyn Error>> {
    let mut new = obligation(id, format!("Representative {id} state"), NOW, false);
    new.retry = RetryPolicy {
        max_attempts,
        base_delay_seconds: if id == "retry-scheduled" {
            315_360_000
        } else {
            600
        },
        max_delay_seconds: 315_360_000,
    };
    store.create(new, NOW - 10)?;
    let claim = store.claim_due(NOW, 60, 1)?.remove(0);
    store.complete(
        &claim,
        Completion::Failed {
            retryable,
            error: format!(
                "Deterministic {id} failure with long diagnostic {}",
                "e".repeat(72)
            ),
            evidence: Some(format!("evidence:{id}:{}", "f".repeat(96))),
        },
        NOW + 1,
    )?;
    Ok(())
}

fn seed_gardener_states(store: &mut Store, root: &Path) -> Result<(), Box<dyn Error>> {
    store.register_gardener_repository(
        NewRepositoryRegistration {
            repository: "robchristie/bokkie".to_owned(),
            default_branch: "main".to_owned(),
            checkout_path: root.join("inert-checkout").display().to_string(),
            inspection_recurrence: Recurrence::new("0 0 1 1 *", "UTC")?,
            first_inspection_at: NOW,
        },
        NOW - 20,
    )?;
    let inspection_claim = store.claim_due_gardener(NOW, 10_000, 1)?.remove(0);
    store.start_gardener_inspection(
        &inspection_claim,
        NewGardenerInspection {
            id: "qualification-inspection".to_owned(),
            source_commit: "1".repeat(40),
            worktree_path: root.join("inert-inspection").display().to_string(),
            prompt_digest: "2".repeat(64),
        },
        NOW + 1,
    )?;
    let prompts = [
        "Review the operator projection and implement the smallest exact-head improvement.\n\nPreserve the trust boundary and retain exact source, tool, environment and input identities. This intentionally long immutable prompt must wrap and remain selectable in the confirmation surface.",
        "Blocking verification fixture proposal",
        "Inconclusive verification fixture proposal",
    ];
    let proposals = store.finish_gardener_inspection(
        &inspection_claim,
        "qualification-inspection",
        &InspectionResult {
            summary: "Three inert UI qualification proposals".to_owned(),
            proposed_goal_prompts: prompts.iter().map(ToString::to_string).collect(),
        },
        NOW + 2,
    )?;
    store.complete(
        &inspection_claim,
        Completion::Succeeded {
            evidence: Some(
                "Fixture-only persisted inspection; no Codex process started".to_owned(),
            ),
        },
        NOW + 3,
    )?;
    for (index, verdict) in [
        GardenerVerificationVerdict::Blocking,
        GardenerVerificationVerdict::Inconclusive,
    ]
    .into_iter()
    .enumerate()
    {
        let proposal = &proposals[index + 1];
        store.decide_gardener_proposal(
            &proposal.fingerprint,
            ApprovalDecision::Approved,
            "qualification-operator",
            Some("Fixture-only approval record; no runner is enabled"),
            NOW + 4 + index as i64,
        )?;
        let claim = store.claim_due_gardener(NOW + 10, 10_000, 1)?.remove(0);
        seed_verification(store, root, &claim, index, verdict)?;
    }
    Ok(())
}

fn seed_verification(
    store: &mut Store,
    root: &Path,
    claim: &bokkie::Claim,
    index: usize,
    verdict: GardenerVerificationVerdict,
) -> Result<(), Box<dyn Error>> {
    let run_id = format!("qualification-run-{index}");
    let head = if index == 0 {
        "a".repeat(40)
    } else {
        "b".repeat(40)
    };
    store.create_gardener_implementation_run(
        claim,
        NewGardenerImplementationRun {
            id: run_id.clone(),
            implementation_worktree_path: root
                .join(format!("inert-implementation-{index}"))
                .display()
                .to_string(),
            branch: format!("codex/gardener-inert-qualification-{index}"),
        },
        NOW + 20,
    )?;
    store.record_implementation_codex_thread(
        claim,
        &run_id,
        &format!("inert-thread-{index}"),
        NOW + 21,
    )?;
    store.record_implementation_codex_turn(
        claim,
        &run_id,
        &format!("inert-turn-{index}"),
        NOW + 22,
    )?;
    store.finish_gardener_implementation(
        claim,
        &run_id,
        r#"{"summary":"fixture-only; no process executed","changed_paths":[],"checks":[]}"#,
        NOW + 23,
    )?;
    store.record_gardener_git_commit(claim, &run_id, &head, NOW + 24)?;
    store.record_gardener_candidate_qualification(
        claim,
        &GardenerCandidateQualification {
            run_id: run_id.clone(),
            head: head.clone(),
            diff_manifest_json: "[]".to_owned(),
            tree_manifest_json: "[]".to_owned(),
            checks_json: r#"[{"executable":{},"arguments":[],"duration_millis":0,"status":{"kind":"passed"},"evidence":{}}]"#.to_owned(),
            duration_ms: 0,
            qualified_at: NOW + 24,
        },
        NOW + 24,
    )?;
    store.record_gardener_push_observation(claim, &run_id, &head, NOW + 25)?;
    let number = 900 + index as u64;
    store.record_gardener_ready_pull_request(
        claim,
        &run_id,
        number,
        &format!("https://github.com/robchristie/bokkie/pull/{number}"),
        &head,
        NOW + 26,
    )?;
    store.start_gardener_verification(
        claim,
        &run_id,
        &root
            .join(format!("inert-verification-{index}"))
            .display()
            .to_string(),
        &head,
        NOW + 27,
    )?;
    store.record_verification_codex_thread(
        claim,
        &run_id,
        &format!("inert-review-thread-{index}"),
        NOW + 28,
    )?;
    store.record_verification_codex_turn(
        claim,
        &run_id,
        &format!("inert-review-turn-{index}"),
        NOW + 29,
    )?;
    let summary = match verdict {
        GardenerVerificationVerdict::Blocking => {
            "Blocking finding retained against the exact inert head"
        }
        GardenerVerificationVerdict::Inconclusive => {
            "Inconclusive verification retained against the exact inert head"
        }
        GardenerVerificationVerdict::Pass => unreachable!(),
    };
    store.finish_gardener_verification(claim, &run_id, verdict, &head, summary, NOW + 30)?;
    store.complete(
        claim,
        Completion::Failed {
            retryable: false,
            error: summary.to_owned(),
            evidence: Some(format!("reported_head={head}; tool=inert-fixture")),
        },
        NOW + 31,
    )?;
    Ok(())
}

fn seed_large(store: &mut Store) -> Result<(), Box<dyn Error>> {
    for index in 0..5_000 {
        store.create(
            obligation(
                &format!("large-{index:05}"),
                format!("Large deterministic ledger row {index:05}"),
                FUTURE + i64::from(index),
                false,
            ),
            NOW,
        )?;
    }
    Ok(())
}

fn seed_empty_inbox(store: &mut Store) -> Result<(), Box<dyn Error>> {
    store.create(
        obligation(
            "completed-only",
            "Terminal obligation outside the exception inbox".to_owned(),
            NOW,
            false,
        ),
        NOW - 10,
    )?;
    let claim = store.claim_due(NOW, 60, 1)?.remove(0);
    store.complete(
        &claim,
        Completion::Succeeded {
            evidence: Some("Empty inbox fixture".to_owned()),
        },
        NOW + 1,
    )?;
    Ok(())
}
