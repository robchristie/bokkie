use super::*;
use crate::workspace::WorkspaceHostProject;
use tempfile::TempDir;

fn empty() -> WorkspaceExchangeRequest {
    WorkspaceExchangeRequest {
        events: vec![],
        heartbeats: vec![],
    }
}
fn setup(store: &mut Store, recurring: bool) -> (WorkspaceHost, String) {
    let project = store
        .workspace_project_save(
            &ProjectSaveRequest {
                command_id: Uuid::new_v4().to_string(),
                project_id: Uuid::new_v4().to_string(),
                expected_revision: 0,
                registration: ProjectRegistration {
                    name: "Bokkie".into(),
                    host: "Nostromo".into(),
                    workspace: "/workspaces/bokkie-workspace".into(),
                    context: "Read the product entry".into(),
                    ..Default::default()
                },
            },
            0,
        )
        .unwrap();
    let limits = WorkspaceLimits {
        max_seconds: 3600,
        max_turns: 10,
        max_tokens: 100000,
    };
    let host = WorkspaceHost {
        id: "nostromo".into(),
        name: "Nostromo".into(),
        token_sha256: "a".repeat(64),
        projects: vec![WorkspaceHostProject {
            project_id: project.id.clone(),
            profile_revision: format!("workspace-v1/{}", project.id),
            permitted_actions: vec!["ordinary_code_delivery".into()],
            limits: limits.clone(),
        }],
    };
    let mut def =
        ManagedTaskDefinition::local_note("Make a small correction", "Use the workspace guidance");
    let profile = host.projects[0].capability_profile(&host.name);
    def.capability = profile.capability.clone();
    def.profile_revision = profile.revision.clone();
    def.effects = profile.effects.clone();
    def.destination = profile.destination.clone();
    def.max_attempts = 1;
    if recurring {
        def.trigger = ManagedTrigger::Recurring {
            cron: "* * * * *".into(),
            timezone: "UTC".into(),
        };
    }
    def.workspace = Some(WorkspaceTaskDefinition {
        project,
        brief: HandoffBrief {
            outcome: "Make a small correction".into(),
            constraints: "Ordinary source changes only".into(),
            acceptance: "Land the reviewed change".into(),
            ..Default::default()
        },
        criteria: vec![WorkspaceCriterion {
            id: "delivery".into(),
            description: "Land the correction".into(),
        }],
        permitted_actions: vec!["ordinary_code_delivery".into()],
        decision_rules: "Ask about wider authority".into(),
        limits,
        review_retained_work: None,
    });
    let id = store
        .managed_create(&Uuid::new_v4().to_string(), &def, 0)
        .unwrap()
        .task_id;
    let preview = store
        .managed_preview(&id, "session", std::slice::from_ref(&profile), 0)
        .unwrap();
    assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
    store
        .managed_activate(
            &Uuid::new_v4().to_string(),
            &preview,
            "session",
            &[profile],
            0,
        )
        .unwrap();
    (host, id)
}
fn dispatch(store: &mut Store, host: &WorkspaceHost, now: i64) -> WorkspaceDispatch {
    store
        .workspace_exchange(host, &empty(), now)
        .unwrap()
        .dispatches
        .into_iter()
        .next()
        .unwrap()
}
fn event(dispatch: &WorkspaceDispatch, sequence: i64, event: WorkspaceEvent) -> WorkspaceHostEvent {
    WorkspaceHostEvent {
        execution_id: dispatch.execution_id.clone(),
        sequence,
        event,
    }
}
fn send(
    store: &mut Store,
    host: &WorkspaceHost,
    event: WorkspaceHostEvent,
    now: i64,
) -> Result<WorkspaceExchangeResponse, StoreError> {
    store.workspace_exchange(
        host,
        &WorkspaceExchangeRequest {
            events: vec![event],
            heartbeats: vec![],
        },
        now,
    )
}
fn result() -> WorkspaceResult {
    WorkspaceResult {
        summary: "Correction reviewed and delivered".into(),
        criteria: vec![WorkspaceCriterionResult {
            id: "delivery".into(),
            satisfied: true,
            evidence: vec!["The admitted correction is in the exact merged tree".into()],
        }],
        deliveries: vec![WorkspaceDelivery {
            repository: "robchristie/bokkie".into(),
            pull_request: "https://github.com/robchristie/bokkie/pull/123".into(),
            reviewed_head: "a".repeat(40),
            merge_revision: "b".repeat(40),
            tree: "c".repeat(40),
            checks: vec!["Exact merge revision backend CI passed".into()],
        }],
        limitations: vec![],
    }
}
fn stopped(result: Option<WorkspaceResult>, passed: bool) -> WorkspaceEvent {
    WorkspaceEvent::Stopped {
        cessation: WorkspaceCessation {
            boundary_id: "owned-pid-namespace-123".into(),
            kind: "descendants_reaped".into(),
            evidence: "The trusted host reaped all owned descendants".into(),
        },
        result,
        verification: Some(WorkspaceVerification {
            passed,
            evidence: vec![
                "Host acquired exact reviewed head, merged tree and post-merge check observations"
                    .into(),
            ],
        }),
        reason: "Workspace boundary stopped".into(),
    }
}
fn action(
    dispatch: &WorkspaceDispatch,
    sequence: i64,
    answer: Option<WorkspaceAnswer>,
    cancel: bool,
) -> WorkspaceRunActionRequest {
    WorkspaceRunActionRequest {
        command_id: Uuid::new_v4().to_string(),
        conversation_id: "conversation".into(),
        execution_id: dispatch.execution_id.clone(),
        expected_event_sequence: sequence,
        answer,
        cancel,
    }
}

fn recovered(
    dispatch: &WorkspaceDispatch,
    result: WorkspaceResult,
    recovered_at: i64,
    passed: bool,
) -> WorkspaceEvent {
    let WorkspaceEvent::Stopped { cessation, .. } = stopped(None, false) else {
        unreachable!()
    };
    WorkspaceEvent::RecoveredResult {
        provenance: WorkspaceRecoveryProvenance {
            origin: "host_reconciliation".into(),
            algorithm: "retained-delivery-v1".into(),
            recovered_at,
            dispatch_digest: canonical_digest(dispatch).unwrap(),
            admission_digest: "d".repeat(64),
            result_digest: canonical_digest(&result).unwrap(),
            cessation,
            sources: vec![
                WorkspaceRecoverySource {
                    kind: "command_observations".into(),
                    sha256: "e".repeat(64),
                },
                WorkspaceRecoverySource {
                    kind: "independent_review".into(),
                    sha256: "f".repeat(64),
                },
                WorkspaceRecoverySource {
                    kind: "criterion_mapping".into(),
                    sha256: "0".repeat(64),
                },
            ],
        },
        result,
        verification: Some(WorkspaceVerification {
            passed,
            evidence: vec![
                "Synthetic trusted host independently inspected the retained delivery".into(),
            ],
        }),
    }
}

fn evidence_review_definition(
    store: &Store,
    id: &str,
    source: &WorkspaceDispatch,
) -> ManagedTaskDefinition {
    let retained = store
        .workspace_run(&source.execution_id)
        .unwrap()
        .result
        .unwrap();
    let mut definition = store.managed_detail(id).unwrap().active.unwrap().definition;
    definition.trigger = ManagedTrigger::Immediate;
    let assignment = definition.workspace.as_mut().unwrap();
    assignment.criteria = vec![WorkspaceCriterion {
        id: "retained-delivery".into(),
        description:
            "Accept the retained reviewed delivery under explicit evidence-only completion criteria"
                .into(),
    }];
    assignment.brief.acceptance = assignment.criteria[0].description.clone();
    assignment.permitted_actions = vec!["inspect".into(), "verify".into()];
    assignment.review_retained_work=Some(WorkspaceEvidenceReview {
        source:WorkspaceEvidenceSource {execution_id:source.execution_id.clone(),result_digest:canonical_digest(&retained).unwrap()},
        summary:"Retained delivery reviewed under the new completion criteria".into(),
        criteria:vec![WorkspaceCriterionResult {id:"retained-delivery".into(),satisfied:true,evidence:vec!["Explicit revised mapping to retained command observations and independent delivery review".into()]}],
    });
    definition
}

fn source_for_evidence_review(store: &mut Store) -> (WorkspaceHost, String, WorkspaceDispatch) {
    let (mut host, id) = setup(store, false);
    let source = dispatch(store, &host, 1);
    send(store, &host, event(&source, 1, stopped(None, false)), 2).unwrap();
    let mut retained = result();
    retained.criteria[0].satisfied = false;
    retained.limitations = vec![
        "The original result tool receipt is absent; original completion remains unproved".into(),
    ];
    send(
        store,
        &host,
        event(&source, 2, recovered(&source, retained, 3, true)),
        3,
    )
    .unwrap();
    host.projects[0]
        .permitted_actions
        .extend(["inspect".into(), "verify".into()]);
    (host, id, source)
}

fn activate_evidence_review(
    store: &mut Store,
    host: &WorkspaceHost,
    id: &str,
    definition: &ManagedTaskDefinition,
    now: i64,
) -> WorkspaceDispatch {
    let expected = store.managed_detail(id).unwrap().configuration_revision;
    store
        .managed_revise(&Uuid::new_v4().to_string(), id, expected, definition, now)
        .unwrap();
    let profile = host.projects[0].capability_profile(&host.name);
    let preview = store
        .managed_preview(id, "session", std::slice::from_ref(&profile), now)
        .unwrap();
    assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
    store
        .managed_activate(
            &Uuid::new_v4().to_string(),
            &preview,
            "session",
            &[profile],
            now,
        )
        .unwrap();
    dispatch(store, host, now + 1)
}

fn evidence_review_stop(
    dispatch: &WorkspaceDispatch,
    result: WorkspaceResult,
    passed: bool,
) -> WorkspaceEvent {
    WorkspaceEvent::Stopped {
        cessation:WorkspaceCessation {boundary_id:format!("review-no-runtime/{}",dispatch.execution_id),kind:"not_started".into(),evidence:"The closed evidence review never launched a workspace implementation runtime".into()},
        result:Some(result),verification:Some(WorkspaceVerification {passed,evidence:vec!["Trusted host independently checked the exact retained delivery and admitted evidence mapping".into()]}),
        reason:"Evidence-only review finished without workspace execution".into(),
    }
}

#[test]
fn explicitly_closed_budget_stop_gets_one_evidence_review_without_rewriting_its_history() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id, source) = source_for_evidence_review(&mut store);
    let source_report = store.workspace_run(&source.execution_id).unwrap();
    assert_eq!(source_report.status, "attention");
    assert!(!source_report.result.as_ref().unwrap().criteria[0].satisfied);
    let definition = evidence_review_definition(&store, &id, &source);
    store
        .managed_revise("review-before-closure", &id, 2, &definition, 4)
        .unwrap();
    let profile = host.projects[0].capability_profile(&host.name);
    let preview = store
        .managed_preview(&id, "session", std::slice::from_ref(&profile), 4)
        .unwrap();
    assert!(
        preview
            .blockers
            .iter()
            .any(|reason| reason.contains("explicitly closed"))
    );
    assert!(
        preview
            .changes
            .iter()
            .any(|change| change.contains("no workspace implementation will run"))
    );
    assert!(
        preview
            .changes
            .iter()
            .any(|change| change.contains("Completion criteria"))
    );
    assert!(
        store
            .managed_activate(
                "cannot-close-implicitly",
                &preview,
                "session",
                std::slice::from_ref(&profile),
                4
            )
            .is_err()
    );
    assert_eq!(
        store.workspace_run(&source.execution_id).unwrap(),
        source_report
    );
    store
        .workspace_run_action(&action(&source, 2, None, true), 5)
        .unwrap();
    let original = store.workspace_run(&source.execution_id).unwrap();
    let retired = store.attempts(&source.obligation_id).unwrap();
    let review = activate_evidence_review(&mut store, &host, &id, &definition, 6);
    assert_ne!(review.execution_id, source.execution_id);
    assert_eq!(review.task_id, source.task_id);
    assert_eq!(
        review.assignment.review_retained_work,
        definition.workspace.as_ref().unwrap().review_retained_work
    );
    assert_eq!(dispatch(&mut store, &host, 8), review);
    assert_eq!(store.attempts(&review.obligation_id).unwrap().len(), 1);
    assert!(
        send(
            &mut store,
            &host,
            event(
                &review,
                1,
                WorkspaceEvent::Started {
                    runtime_id: "forbidden-runtime".into(),
                    instruction_sources: vec!["AGENTS.md".into()]
                }
            ),
            8
        )
        .is_err()
    );
    let expected =
        retained_review_result(&store.connection, &id, &review.assignment, Some(&host.id))
            .unwrap()
            .unwrap();
    assert_eq!(
        expected.deliveries,
        original.result.as_ref().unwrap().deliveries
    );
    assert!(expected.limitations.is_empty());
    let waiting = event(
        &review,
        1,
        evidence_review_stop(&review, expected.clone(), false),
    );
    send(&mut store, &host, waiting.clone(), 8).unwrap();
    assert_eq!(
        store.workspace_run(&review.execution_id).unwrap().status,
        "attention"
    );
    assert!(
        store
            .workspace_exchange(&host, &empty(), 9)
            .unwrap()
            .dispatches
            .is_empty()
    );
    let complete = event(&review, 2, evidence_review_stop(&review, expected, true));
    send(&mut store, &host, complete.clone(), 9).unwrap();
    assert_eq!(
        store.get(&review.obligation_id).unwrap().unwrap().state,
        ObligationState::Completed
    );
    assert_eq!(
        store.managed_detail(&id).unwrap().status,
        ManagedTaskStatus::Completed
    );
    assert_eq!(store.managed_detail(&id).unwrap().runs.len(), 2);
    assert_eq!(store.workspace_run(&source.execution_id).unwrap(), original);
    assert_eq!(store.attempts(&source.obligation_id).unwrap(), retired);
    let through = store.change_page(0, None, 100).unwrap().through;
    send(&mut store, &host, waiting, 10).unwrap();
    send(&mut store, &host, complete, 10).unwrap();
    assert_eq!(store.change_page(0, None, 100).unwrap().through, through);
}

#[test]
fn evidence_review_rejects_cross_task_project_digest_and_unclosed_or_chained_sources() {
    for case in 0..4 {
        let mut store = Store::open_in_memory().unwrap();
        let (host, id, source) = source_for_evidence_review(&mut store);
        if case != 3 {
            store
                .workspace_run_action(&action(&source, 2, None, true), 4)
                .unwrap();
        }
        let mut definition = evidence_review_definition(&store, &id, &source);
        if case == 1 {
            definition
                .workspace
                .as_mut()
                .unwrap()
                .project
                .registration
                .context = "Different exact snapshot".into();
        }
        if case == 2 {
            definition
                .workspace
                .as_mut()
                .unwrap()
                .review_retained_work
                .as_mut()
                .unwrap()
                .source
                .result_digest = "0".repeat(64);
        }
        let target = if case == 0 {
            store
                .managed_create("cross-task", &definition, 5)
                .unwrap()
                .task_id
        } else {
            store
                .managed_revise("candidate", &id, 2, &definition, 5)
                .unwrap()
                .task_id
        };
        let profiles = [host.projects[0].capability_profile(&host.name)];
        let preview = store
            .managed_preview(&target, "session", &profiles, 5)
            .unwrap();
        assert!(!preview.blockers.is_empty());
        assert!(
            store
                .managed_activate("reject-source", &preview, "session", &profiles, 5)
                .is_err()
        );
    }
    let mut store = Store::open_in_memory().unwrap();
    let (host, id, source) = source_for_evidence_review(&mut store);
    store
        .workspace_run_action(&action(&source, 2, None, true), 4)
        .unwrap();
    let definition = evidence_review_definition(&store, &id, &source);
    let review = activate_evidence_review(&mut store, &host, &id, &definition, 5);
    let expected =
        retained_review_result(&store.connection, &id, &review.assignment, Some(&host.id))
            .unwrap()
            .unwrap();
    send(
        &mut store,
        &host,
        event(&review, 1, evidence_review_stop(&review, expected, false)),
        7,
    )
    .unwrap();
    store
        .workspace_run_action(&action(&review, 1, None, true), 8)
        .unwrap();
    let chained = evidence_review_definition(&store, &id, &review);
    let revision = store.managed_detail(&id).unwrap().configuration_revision;
    store
        .managed_revise("chained", &id, revision, &chained, 9)
        .unwrap();
    let preview = store
        .managed_preview(
            &id,
            "session",
            &[host.projects[0].capability_profile(&host.name)],
            9,
        )
        .unwrap();
    assert!(!preview.blockers.is_empty());
}

#[test]
fn evidence_review_closed_shape_rejects_timing_write_actions_and_inexact_mapping() {
    for case in 0..5 {
        let mut store = Store::open_in_memory().unwrap();
        let (_host, id, source) = source_for_evidence_review(&mut store);
        let mut definition = evidence_review_definition(&store, &id, &source);
        match case {
            0 => {
                definition.trigger = ManagedTrigger::Recurring {
                    cron: "* * * * *".into(),
                    timezone: "UTC".into(),
                }
            }
            1 => {
                definition.trigger = ManagedTrigger::Once {
                    local_datetime: "2030-01-01T09:00".into(),
                    timezone: "UTC".into(),
                }
            }
            2 => definition
                .workspace
                .as_mut()
                .unwrap()
                .permitted_actions
                .push("implement".into()),
            3 => definition
                .workspace
                .as_mut()
                .unwrap()
                .review_retained_work
                .as_mut()
                .unwrap()
                .criteria[0]
                .evidence
                .clear(),
            _ => {
                definition
                    .workspace
                    .as_mut()
                    .unwrap()
                    .review_retained_work
                    .as_mut()
                    .unwrap()
                    .criteria[0]
                    .id = "unknown-criterion".into()
            }
        }
        assert!(
            store
                .managed_revise("invalid-mode", &id, 2, &definition, 4)
                .is_err()
        );
    }
}

#[test]
fn retained_evidence_admission_cannot_change_the_authenticated_source_host() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id, source) = source_for_evidence_review(&mut store);
    store
        .workspace_run_action(&action(&source, 2, None, true), 4)
        .unwrap();
    let definition = evidence_review_definition(&store, &id, &source);
    store
        .managed_revise("host-fenced-review", &id, 2, &definition, 5)
        .unwrap();
    let profile = host.projects[0].capability_profile(&host.name);
    let preview = store
        .managed_preview(&id, "session", std::slice::from_ref(&profile), 5)
        .unwrap();
    store
        .managed_activate("activate-host-review", &preview, "session", &[profile], 5)
        .unwrap();
    let mut other = host.clone();
    other.id = "replacement-host".into();
    assert!(store.workspace_exchange(&other, &empty(), 6).is_err());
    let runs = store.managed_detail(&id).unwrap().runs;
    assert_eq!(runs[0].admitted_at, None);
    assert_eq!(runs[0].workspace, None);
    let review = dispatch(&mut store, &host, 7);
    assert_eq!(
        review.assignment.review_retained_work,
        definition.workspace.as_ref().unwrap().review_retained_work
    );
}

#[test]
fn evidence_review_fences_host_result_replacement_and_cancellation_while_ordinary_not_started_cannot_accept()
 {
    for case in 0..5 {
        let mut store = Store::open_in_memory().unwrap();
        let (host, id, source) = source_for_evidence_review(&mut store);
        store
            .workspace_run_action(&action(&source, 2, None, true), 4)
            .unwrap();
        let definition = evidence_review_definition(&store, &id, &source);
        let review = activate_evidence_review(&mut store, &host, &id, &definition, 5);
        let mut expected =
            retained_review_result(&store.connection, &id, &review.assignment, Some(&host.id))
                .unwrap()
                .unwrap();
        let mut actor = host.clone();
        match case {
            0 => actor.id = "another-host".into(),
            1 => expected.summary = "Host-invented summary".into(),
            2 => expected.criteria[0]
                .evidence
                .push("Unreviewed replacement mapping".into()),
            3 => expected.deliveries[0].merge_revision = "d".repeat(40),
            _ => store
                .workspace_run_action(&action(&review, 0, None, true), 7)
                .unwrap(),
        };
        let stop = event(&review, 1, evidence_review_stop(&review, expected, true));
        if case == 4 {
            send(&mut store, &actor, stop, 8).unwrap();
            assert_eq!(
                store.get(&review.obligation_id).unwrap().unwrap().state,
                ObligationState::Cancelled
            );
        } else {
            assert!(send(&mut store, &actor, stop, 8).is_err());
            assert_eq!(
                store
                    .workspace_run(&review.execution_id)
                    .unwrap()
                    .last_event_sequence,
                0
            );
        }
    }
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let ordinary = dispatch(&mut store, &host, 1);
    send(
        &mut store,
        &host,
        event(
            &ordinary,
            1,
            evidence_review_stop(&ordinary, result(), true),
        ),
        2,
    )
    .unwrap();
    assert_eq!(
        store.get(&ordinary.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
}

#[test]
fn canonical_recovery_digest_matches_sorted_compact_unicode_json() {
    let value = json!({"z":"é","a":[{"β":"澳","a":1}]});
    assert_eq!(
        canonical_digest(&value).unwrap(),
        "68e1ec55fab4afde87f531772e53e06a4fb15c35f3e9439fbc286120e15a0a69"
    );
}

#[test]
fn budget_stop_recovers_additively_and_verifies_the_same_result_once() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    send(
        &mut store,
        &host,
        event(
            &d,
            1,
            WorkspaceEvent::Started {
                runtime_id: "runtime-budget".into(),
                instruction_sources: vec!["AGENTS.md".into()],
            },
        ),
        2,
    )
    .unwrap();
    let mut budget_stop = stopped(None, false);
    if let WorkspaceEvent::Stopped {
        reason,
        verification,
        ..
    } = &mut budget_stop
    {
        *reason = "Finite turn budget exhausted; descendants safely reaped".into();
        *verification = None;
    }
    let budget_event = event(&d, 2, budget_stop.clone());
    send(&mut store, &host, budget_event.clone(), 3).unwrap();
    let retired_attempts = store.attempts(&d.obligation_id).unwrap();
    let retained = result();
    let recovery_event = event(
        &d,
        3,
        recovered(&d, retained.clone(), d.deadline_at + 1, false),
    );
    let response = send(&mut store, &host, recovery_event.clone(), d.deadline_at + 2).unwrap();
    assert!(response.dispatches.is_empty());
    assert!(response.controls.is_empty());
    let run = store.workspace_run(&d.execution_id).unwrap();
    assert_eq!(run.result, Some(retained.clone()));
    assert_eq!(run.status, "attention");
    assert!(!run.verification.as_ref().unwrap().passed);
    assert!(run.cessation_verified);
    assert_eq!(run.recovery.as_ref().unwrap().origin, "host_reconciliation");
    assert_eq!(
        store.managed_detail(&id).unwrap().runs[0]
            .workspace
            .as_ref()
            .unwrap()
            .recovery,
        run.recovery
    );
    let original: Option<String> = store
        .connection
        .query_row(
            "SELECT result_json FROM workspace_executions WHERE execution_id=?1",
            [&d.execution_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(original.is_none());
    let original_event:String=store.connection.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND sequence=2",[&d.execution_id],|r|r.get(0)).unwrap();
    assert_eq!(
        decode::<WorkspaceEvent>(&original_event).unwrap(),
        budget_stop
    );
    assert!(
        store
            .connection
            .execute(
                "UPDATE workspace_execution_recoveries SET result_json='{}' WHERE execution_id=?1",
                [&d.execution_id]
            )
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(
                "DELETE FROM workspace_execution_recoveries WHERE execution_id=?1",
                [&d.execution_id]
            )
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(
                "UPDATE workspace_executions SET result_json='{}' WHERE execution_id=?1",
                [&d.execution_id]
            )
            .is_err()
    );
    let through = store.change_page(0, None, 100).unwrap().through;
    send(&mut store, &host, recovery_event.clone(), d.deadline_at + 3).unwrap();
    send(&mut store, &host, budget_event, d.deadline_at + 3).unwrap();
    assert_eq!(store.change_page(0, None, 100).unwrap().through, through);
    let mut changed = recovery_event.clone();
    if let WorkspaceEvent::RecoveredResult { verification, .. } = &mut changed.event {
        verification.as_mut().unwrap().passed = true;
    }
    assert!(send(&mut store, &host, changed, d.deadline_at + 3).is_err());
    assert!(
        send(
            &mut store,
            &host,
            event(&d, 4, recovery_event.event.clone()),
            d.deadline_at + 3
        )
        .is_err()
    );
    let verified = event(&d, 4, stopped(Some(retained.clone()), true));
    send(&mut store, &host, verified.clone(), d.deadline_at + 4).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Completed
    );
    assert_eq!(
        store.managed_detail(&id).unwrap().status,
        ManagedTaskStatus::Completed
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap(), retired_attempts);
    assert!(
        store
            .workspace_run(&d.execution_id)
            .unwrap()
            .verification
            .unwrap()
            .passed
    );
    assert_eq!(
        execution(&store.connection, &d.execution_id)
            .unwrap()
            .dispatch,
        d
    );
    let through = store.change_page(0, None, 100).unwrap().through;
    send(&mut store, &host, verified, d.deadline_at + 5).unwrap();
    send(&mut store, &host, recovery_event, d.deadline_at + 5).unwrap();
    assert_eq!(store.change_page(0, None, 100).unwrap().through, through);
    let original: Option<String> = store
        .connection
        .query_row(
            "SELECT result_json FROM workspace_executions WHERE execution_id=?1",
            [&d.execution_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(original.is_none());
    let count: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM managed_results WHERE obligation_id=?1",
            [&d.obligation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(
        send(
            &mut store,
            &host,
            event(&d, 5, stopped(Some(retained), true)),
            d.deadline_at + 5
        )
        .is_err()
    );
}

#[test]
fn recovery_rejects_wrong_host_cancellation_and_existing_results_without_mutation() {
    for case in 0..5 {
        let mut store = Store::open_in_memory().unwrap();
        let (host, _) = setup(&mut store, false);
        let d = dispatch(&mut store, &host, 1);
        let mut stop = stopped(if case == 2 { Some(result()) } else { None }, false);
        if case == 3 {
            if let WorkspaceEvent::Stopped { cessation, .. } = &mut stop {
                cessation.kind = "not_started".into();
            }
        }
        if case == 4 {
            send(
                &mut store,
                &host,
                event(
                    &d,
                    1,
                    WorkspaceEvent::Question {
                        question: WorkspaceQuestion {
                            id: "missing-context".into(),
                            kind: "missing_information".into(),
                            prompt: "Which exact target?".into(),
                            options: vec![],
                        },
                    },
                ),
                2,
            )
            .unwrap();
        }
        let sequence = if case == 4 { 2 } else { 1 };
        send(&mut store, &host, event(&d, sequence, stop), 3).unwrap();
        if case == 1 {
            store
                .workspace_run_action(&action(&d, sequence, None, true), 4)
                .unwrap();
        }
        let mut actor = host.clone();
        if case == 0 {
            actor.id = "another-host".into();
        }
        let before = store.workspace_run(&d.execution_id).unwrap();
        assert!(
            send(
                &mut store,
                &actor,
                event(&d, sequence + 1, recovered(&d, result(), 4, true)),
                5
            )
            .is_err()
        );
        assert_eq!(store.workspace_run(&d.execution_id).unwrap(), before);
        let count: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM workspace_execution_recoveries WHERE execution_id=?1",
                [&d.execution_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn recovery_retains_unsatisfied_original_criteria_and_named_limitations_in_attention() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    send(&mut store, &host, event(&d, 1, stopped(None, false)), 2).unwrap();
    let mut retained = result();
    retained.criteria[0].satisfied = false;
    retained.limitations.push("The original admission required a result tool receipt that is absent from retained evidence".into());
    send(
        &mut store,
        &host,
        event(&d, 2, recovered(&d, retained.clone(), 3, true)),
        3,
    )
    .unwrap();
    let run = store.workspace_run(&d.execution_id).unwrap();
    assert_eq!(run.result, Some(retained.clone()));
    assert!(run.recovery.is_some());
    assert_eq!(run.status, "attention");
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    send(
        &mut store,
        &host,
        event(&d, 3, stopped(Some(retained.clone()), true)),
        4,
    )
    .unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap().len(), 1);
    send(
        &mut store,
        &host,
        event(&d, 4, stopped(Some(retained.clone()), false)),
        5,
    )
    .unwrap();
    assert!(
        !store
            .workspace_run(&d.execution_id)
            .unwrap()
            .verification
            .unwrap()
            .passed
    );
    let mut unavailable = stopped(Some(retained), false);
    if let WorkspaceEvent::Stopped { verification, .. } = &mut unavailable {
        *verification = None;
    }
    send(&mut store, &host, event(&d, 5, unavailable), 6).unwrap();
    assert!(
        store
            .workspace_run(&d.execution_id)
            .unwrap()
            .verification
            .is_none()
    );
}

#[test]
fn recovery_digests_provenance_and_replacements_are_fenced_atomically() {
    for case in 0..9 {
        let mut store = Store::open_in_memory().unwrap();
        let (host, _) = setup(&mut store, false);
        let d = dispatch(&mut store, &host, 1);
        send(&mut store, &host, event(&d, 1, stopped(None, false)), 2).unwrap();
        let mut recovery = recovered(&d, result(), 3, false);
        if let WorkspaceEvent::RecoveredResult { provenance, .. } = &mut recovery {
            match case {
                0 => provenance.dispatch_digest = "0".repeat(64),
                1 => provenance.result_digest = "1".repeat(64),
                2 => provenance.admission_digest = "unchecked receipt".into(),
                3 => provenance.cessation.boundary_id = "different-boundary".into(),
                4 => provenance.origin = "agent_claim".into(),
                5 => provenance.sources.clear(),
                6 => provenance.sources[0].sha256 = "A".repeat(64),
                7 => provenance.sources = vec![provenance.sources[0].clone(); 17],
                _ => provenance.recovered_at = 100,
            }
        }
        let before = store.workspace_run(&d.execution_id).unwrap();
        assert!(send(&mut store, &host, event(&d, 2, recovery), 4).is_err());
        assert_eq!(store.workspace_run(&d.execution_id).unwrap(), before);
    }
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    send(&mut store, &host, event(&d, 1, stopped(None, false)), 2).unwrap();
    send(
        &mut store,
        &host,
        event(&d, 2, recovered(&d, result(), 3, false)),
        3,
    )
    .unwrap();
    let before = store.workspace_run(&d.execution_id).unwrap();
    let mut replacement = result();
    replacement.summary = "Replacement cannot overwrite recovered evidence".into();
    assert!(
        send(
            &mut store,
            &host,
            event(&d, 3, recovered(&d, replacement.clone(), 4, true)),
            4
        )
        .is_err()
    );
    assert!(
        send(
            &mut store,
            &host,
            event(&d, 3, stopped(Some(replacement), true)),
            4
        )
        .is_err()
    );
    assert_eq!(store.workspace_run(&d.execution_id).unwrap(), before);
}

#[test]
fn admission_and_restart_replay_the_identical_dispatch_without_another_attempt() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("store.sqlite");
    let mut store = Store::open(&path).unwrap();
    let (host, id) = setup(&mut store, false);
    let admitted = dispatch(&mut store, &host, 1);
    assert_eq!(admitted.admitted_at, 1);
    assert_eq!(admitted.deadline_at, 3601);
    assert_eq!(dispatch(&mut store, &host, 2), admitted);
    let state = store.managed_detail(&id).unwrap();
    assert_eq!(state.runs[0].admitted_at, Some(1));
    assert_eq!(store.attempts(&admitted.obligation_id).unwrap().len(), 1);
    drop(store);
    let mut store = Store::open_compatible(&path).unwrap();
    assert_eq!(dispatch(&mut store, &host, 3), admitted);
    assert_eq!(store.attempts(&admitted.obligation_id).unwrap().len(), 1);
}

#[test]
fn host_events_reject_gaps_cross_host_and_changed_replays_atomically() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let start = event(
        &d,
        1,
        WorkspaceEvent::Started {
            runtime_id: "runtime-1".into(),
            instruction_sources: vec!["AGENTS.md".into()],
        },
    );
    let mut other = host.clone();
    other.id = "other-host".into();
    assert!(send(&mut store, &other, start.clone(), 2).is_err());
    assert!(
        send(
            &mut store,
            &host,
            event(
                &d,
                2,
                WorkspaceEvent::Progress {
                    summary: "Too early".into()
                }
            ),
            2
        )
        .is_err()
    );
    assert_eq!(
        store
            .workspace_run(&d.execution_id)
            .unwrap()
            .last_event_sequence,
        0
    );
    send(&mut store, &host, start.clone(), 2).unwrap();
    let through = store.change_page(0, None, 100).unwrap().through;
    send(&mut store, &host, start.clone(), 2).unwrap();
    assert_eq!(store.change_page(0, None, 100).unwrap().through, through);
    let changed = event(
        &d,
        1,
        WorkspaceEvent::Progress {
            summary: "Changed replay".into(),
        },
    );
    assert!(send(&mut store, &host, changed, 2).is_err());
    let batch = WorkspaceExchangeRequest {
        events: vec![
            event(
                &d,
                2,
                WorkspaceEvent::Progress {
                    summary: "First".into(),
                },
            ),
            event(
                &d,
                4,
                WorkspaceEvent::Progress {
                    summary: "Gap".into(),
                },
            ),
        ],
        heartbeats: vec![],
    };
    assert!(store.workspace_exchange(&host, &batch, 3).is_err());
    assert_eq!(
        store
            .workspace_run(&d.execution_id)
            .unwrap()
            .last_event_sequence,
        1
    );
    send(
        &mut store,
        &host,
        event(&d, 2, stopped(Some(result()), true)),
        3,
    )
    .unwrap();
    send(&mut store, &host, start, 3).unwrap();
    assert!(
        send(
            &mut store,
            &host,
            event(
                &d,
                3,
                WorkspaceEvent::Progress {
                    summary: "After terminal".into()
                }
            ),
            3
        )
        .is_err()
    );
}

#[test]
fn snapshot_and_profile_fences_preserve_admitted_work_across_edits_and_pause() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let project = d.assignment.project.clone();
    let mut registration = project.registration.clone();
    registration.workspace = "/workspaces/other".into();
    store
        .workspace_project_save(
            &ProjectSaveRequest {
                command_id: "edit-project".into(),
                project_id: project.id,
                expected_revision: project.revision,
                registration,
            },
            2,
        )
        .unwrap();
    let mut def = store
        .managed_detail(&id)
        .unwrap()
        .active
        .unwrap()
        .definition;
    def.purpose = "Changed future task".into();
    store.managed_revise("revise", &id, 2, &def, 3).unwrap();
    let preview = store
        .managed_preview(
            &id,
            "session",
            &[host.projects[0].capability_profile(&host.name)],
            3,
        )
        .unwrap();
    assert!(!preview.blockers.is_empty());
    assert!(
        store
            .managed_activate(
                "stale-snapshot",
                &preview,
                "session",
                &[host.projects[0].capability_profile(&host.name)],
                3
            )
            .is_err()
    );
    store.managed_pause("pause", &id, 3, 4).unwrap();
    assert_eq!(dispatch(&mut store, &host, 5), d);
    let mut wrong = host.clone();
    wrong.projects[0].profile_revision = "changed-profile".into();
    assert_eq!(dispatch(&mut store, &wrong, 5), d); // already admitted ownership is immutable
    assert_eq!(store.managed_detail(&id).unwrap().runs.len(), 1);
}

#[test]
fn expired_ownership_reconciles_the_same_execution_and_retains_retired_attempts() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    assert_eq!(store.recover_expired_leases(91).unwrap(), 1);
    let retired = store.attempts(&d.obligation_id).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    assert!(store.claim_due(92, 30, 10).unwrap().is_empty());
    assert!(store.claim_due_notes(92, 30, 10).unwrap().is_empty());
    let response = store
        .workspace_exchange(
            &host,
            &WorkspaceExchangeRequest {
                events: vec![],
                heartbeats: vec![d.execution_id.clone()],
            },
            92,
        )
        .unwrap();
    assert_eq!(response.dispatches.as_slice(), std::slice::from_ref(&d));
    assert_eq!(store.attempts(&d.obligation_id).unwrap(), retired);
    let o = store.get(&d.obligation_id).unwrap().unwrap();
    assert_eq!(o.state, ObligationState::Pending);
    assert_eq!(o.next_wake_at, Some(182));
    assert_eq!(o.attempts_made, 1);
    assert_eq!(dispatch(&mut store, &host, 100), d);
    assert_eq!(store.attempts(&d.obligation_id).unwrap(), retired);
    store.recover_expired_leases(182).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    send(
        &mut store,
        &host,
        event(&d, 1, stopped(Some(result()), true)),
        183,
    )
    .unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Completed
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap(), retired);
}

#[test]
fn cancellation_is_monotonic_and_terminal_only_after_verified_cessation() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let cancel = action(&d, 0, None, true);
    store.workspace_run_action(&cancel, 2).unwrap();
    store.workspace_run_action(&cancel, 3).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    let response = store.workspace_exchange(&host, &empty(), 3).unwrap();
    assert!(response.controls[0].cancel);
    let mut changed = cancel.clone();
    changed.expected_event_sequence = 1;
    assert!(store.workspace_run_action(&changed, 3).is_err());
    let progress = event(
        &d,
        1,
        WorkspaceEvent::Progress {
            summary: "Still writing".into(),
        },
    );
    let response = send(&mut store, &host, progress.clone(), 3).unwrap();
    assert_eq!(response.acknowledgements[0].sequence, 1);
    assert!(response.controls[0].cancel);
    assert_eq!(
        store.workspace_run(&d.execution_id).unwrap().status,
        "cancelling"
    );
    let mut invalid_stop = stopped(None, false);
    if let WorkspaceEvent::Stopped { cessation, .. } = &mut invalid_stop {
        cessation.kind = "process_exited".into();
    }
    assert!(send(&mut store, &host, event(&d, 2, invalid_stop), 4).is_err());
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    let stop = event(&d, 2, stopped(Some(result()), true));
    send(&mut store, &host, stop.clone(), 4).unwrap();
    send(&mut store, &host, stop, 5).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Cancelled
    );
    let retained:String=store.connection.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND sequence=1",[&d.execution_id],|row|row.get(0)).unwrap();
    assert_eq!(decode::<WorkspaceEvent>(&retained).unwrap(), progress.event);
    assert!(
        store
            .workspace_exchange(&host, &empty(), 5)
            .unwrap()
            .dispatches
            .is_empty()
    );
}

#[test]
fn cancellation_acknowledges_the_immutable_queued_journal_before_later_cessation() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    send(
        &mut store,
        &host,
        event(
            &d,
            1,
            WorkspaceEvent::Started {
                runtime_id: "owned-runtime".into(),
                instruction_sources: vec!["AGENTS.md".into()],
            },
        ),
        2,
    )
    .unwrap();
    // These observations already exist in the host journal before the operator
    // cancels at the last acknowledged cursor. Their payloads cannot be replaced.
    let queued = vec![
        event(
            &d,
            2,
            WorkspaceEvent::Progress {
                summary: "A retained turn is finishing".into(),
            },
        ),
        event(
            &d,
            3,
            WorkspaceEvent::Question {
                question: WorkspaceQuestion {
                    id: "queued-question".into(),
                    kind: "missing_information".into(),
                    prompt: "Which bounded source should I inspect?".into(),
                    options: vec![],
                },
            },
        ),
        event(
            &d,
            4,
            WorkspaceEvent::Started {
                runtime_id: "owned-runtime".into(),
                instruction_sources: vec!["AGENTS.md".into()],
            },
        ),
        event(
            &d,
            5,
            WorkspaceEvent::Question {
                question: WorkspaceQuestion {
                    id: "queued-question".into(),
                    kind: "new_authority".into(),
                    prompt: "A late question requests wider scope".into(),
                    options: vec![],
                },
            },
        ),
        event(
            &d,
            6,
            WorkspaceEvent::Attention {
                reason: "A late runtime observation needs reconciliation".into(),
            },
        ),
    ];
    store
        .workspace_run_action(&action(&d, 1, None, true), 3)
        .unwrap();
    let mut expected = store.workspace_run(&d.execution_id).unwrap();
    let responsibility = store.get(&d.obligation_id).unwrap();
    let attempt = store.attempts(&d.obligation_id).unwrap();
    let batch = WorkspaceExchangeRequest {
        events: queued.clone(),
        heartbeats: vec![d.execution_id.clone()],
    };
    let response = store.workspace_exchange(&host, &batch, 4).unwrap();
    assert_eq!(
        response
            .acknowledgements
            .iter()
            .map(|ack| ack.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3, 4, 5, 6]
    );
    assert_eq!(response.controls.len(), 1);
    assert!(response.controls[0].cancel);
    assert!(response.controls[0].answers.is_empty());
    expected.last_event_sequence = 6;
    assert_eq!(store.workspace_run(&d.execution_id).unwrap(), expected);
    assert_eq!(store.get(&d.obligation_id).unwrap(), responsibility);
    assert_eq!(store.attempts(&d.obligation_id).unwrap(), attempt);
    let questions: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM workspace_execution_questions WHERE execution_id=?1",
            [&d.execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(questions, 0);
    assert!(
        store
            .workspace_run_action(
                &action(
                    &d,
                    6,
                    Some(WorkspaceAnswer {
                        question_id: "queued-question".into(),
                        text: "A wider scope is forbidden".into()
                    }),
                    false
                ),
                4
            )
            .is_err()
    );
    for observation in &queued {
        let retained:String=store.connection.query_row("SELECT event_json FROM workspace_execution_events WHERE execution_id=?1 AND sequence=?2",params![d.execution_id,observation.sequence],|row|row.get(0)).unwrap();
        assert_eq!(
            decode::<WorkspaceEvent>(&retained).unwrap(),
            observation.event
        );
    }
    let through = store.change_page(0, None, 100).unwrap().through;
    store.workspace_exchange(&host, &batch, 5).unwrap();
    assert_eq!(store.change_page(0, None, 100).unwrap().through, through);
    let mut conflict = queued[0].clone();
    conflict.event = WorkspaceEvent::Progress {
        summary: "Changed immutable journal payload".into(),
    };
    assert!(send(&mut store, &host, conflict, 5).is_err());
    assert_eq!(store.workspace_run(&d.execution_id).unwrap(), expected);
    let stop = event(&d, 7, stopped(Some(result()), true));
    send(&mut store, &host, stop.clone(), 6).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Cancelled
    );
    assert_eq!(
        store.workspace_run(&d.execution_id).unwrap().result,
        Some(result())
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap(), attempt);
    let accepted: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM managed_results WHERE obligation_id=?1",
            [&d.obligation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(accepted, 0);
    let mut replay = queued;
    replay.push(stop);
    let through = store.change_page(0, None, 100).unwrap().through;
    let response = store
        .workspace_exchange(
            &host,
            &WorkspaceExchangeRequest {
                events: replay,
                heartbeats: vec![],
            },
            7,
        )
        .unwrap();
    assert_eq!(response.acknowledgements.len(), 6);
    assert!(response.controls.is_empty());
    assert!(response.dispatches.is_empty());
    assert_eq!(store.change_page(0, None, 100).unwrap().through, through);
}

#[test]
fn cancellation_records_duplicate_question_ids_without_reopening_question_responsibility() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let original = WorkspaceQuestion {
        id: "existing-question".into(),
        kind: "missing_information".into(),
        prompt: "Which source needs inspection?".into(),
        options: vec![],
    };
    send(
        &mut store,
        &host,
        event(
            &d,
            1,
            WorkspaceEvent::Question {
                question: original.clone(),
            },
        ),
        2,
    )
    .unwrap();
    store
        .workspace_run_action(&action(&d, 1, None, true), 3)
        .unwrap();
    let queued = vec![
        event(
            &d,
            2,
            WorkspaceEvent::Question {
                question: original.clone(),
            },
        ),
        event(
            &d,
            3,
            WorkspaceEvent::Question {
                question: WorkspaceQuestion {
                    id: original.id.clone(),
                    kind: "new_authority".into(),
                    prompt: "Late replacement must not grant authority".into(),
                    options: vec![],
                },
            },
        ),
    ];
    let response = store
        .workspace_exchange(
            &host,
            &WorkspaceExchangeRequest {
                events: queued,
                heartbeats: vec![],
            },
            4,
        )
        .unwrap();
    assert_eq!(response.acknowledgements.len(), 2);
    assert!(response.controls[0].cancel);
    let run = store.workspace_run(&d.execution_id).unwrap();
    assert_eq!(run.status, "cancelling");
    assert_eq!(run.question, Some(original.clone()));
    let retained:String=store.connection.query_row("SELECT question_json FROM workspace_execution_questions WHERE execution_id=?1 AND question_id=?2",params![d.execution_id,original.id],|row|row.get(0)).unwrap();
    assert_eq!(decode::<WorkspaceQuestion>(&retained).unwrap(), original);
    let count: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM workspace_execution_questions WHERE execution_id=?1",
            [&d.execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(
        store
            .workspace_run_action(
                &action(
                    &d,
                    3,
                    Some(WorkspaceAnswer {
                        question_id: "existing-question".into(),
                        text: "This cannot resolve new authority".into()
                    }),
                    false
                ),
                4
            )
            .is_err()
    );
    send(&mut store, &host, event(&d, 4, stopped(None, false)), 5).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Cancelled
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap().len(), 1);
}

#[test]
fn questions_and_immutable_answers_replay_until_stopped_without_widening_authority() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let q = WorkspaceQuestion {
        id: "which-file".into(),
        kind: "missing_information".into(),
        prompt: "Which file needs the correction?".into(),
        options: vec!["README".into()],
    };
    send(
        &mut store,
        &host,
        event(
            &d,
            1,
            WorkspaceEvent::Question {
                question: q.clone(),
            },
        ),
        2,
    )
    .unwrap();
    assert_eq!(
        store.workspace_run(&d.execution_id).unwrap().question,
        Some(q)
    );
    let answer = WorkspaceAnswer {
        question_id: "which-file".into(),
        text: "README".into(),
    };
    let request = action(&d, 1, Some(answer.clone()), false);
    store.workspace_run_action(&request, 3).unwrap();
    store.workspace_run_action(&request, 4).unwrap();
    assert_eq!(
        store
            .workspace_exchange(&host, &empty(), 4)
            .unwrap()
            .controls[0]
            .answers,
        [answer]
    );
    let mut changed = request.clone();
    changed.command_id = "changed-answer".into();
    changed.answer.as_mut().unwrap().text = "A wider change".into();
    assert!(store.workspace_run_action(&changed, 4).is_err());
    send(
        &mut store,
        &host,
        event(
            &d,
            2,
            WorkspaceEvent::Question {
                question: WorkspaceQuestion {
                    id: "deploy".into(),
                    kind: "new_authority".into(),
                    prompt: "May I deploy?".into(),
                    options: vec![],
                },
            },
        ),
        5,
    )
    .unwrap();
    let request = action(
        &d,
        2,
        Some(WorkspaceAnswer {
            question_id: "deploy".into(),
            text: "Yes".into(),
        }),
        false,
    );
    assert!(store.workspace_run_action(&request, 6).is_err());
    store
        .workspace_exchange(
            &host,
            &WorkspaceExchangeRequest {
                events: vec![],
                heartbeats: vec![d.execution_id.clone()],
            },
            6,
        )
        .unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
}

#[test]
fn insufficient_acceptance_retains_result_and_trusted_reverification_never_reruns_work() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let retained = result();
    send(
        &mut store,
        &host,
        event(&d, 1, stopped(Some(retained.clone()), false)),
        2,
    )
    .unwrap();
    let run = store.workspace_run(&d.execution_id).unwrap();
    assert!(run.cessation_verified);
    assert_eq!(run.result, Some(retained.clone()));
    assert_eq!(run.status, "attention");
    assert_eq!(
        store.managed_detail(&id).unwrap().runs[0].state,
        "attention"
    );
    assert!(
        store
            .workspace_exchange(&host, &empty(), 3)
            .unwrap()
            .dispatches
            .is_empty()
    );
    let mut changed = retained.clone();
    changed.summary = "Replacement result".into();
    assert!(
        send(
            &mut store,
            &host,
            event(&d, 2, stopped(Some(changed), true)),
            3
        )
        .is_err()
    );
    send(
        &mut store,
        &host,
        event(&d, 2, stopped(Some(retained), true)),
        3,
    )
    .unwrap();
    assert_eq!(
        store.managed_detail(&id).unwrap().status,
        ManagedTaskStatus::Completed
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap().len(), 1);
    assert!(
        send(
            &mut store,
            &host,
            event(&d, 3, stopped(Some(result()), true)),
            4
        )
        .is_err()
    );
}

#[test]
fn successful_exit_or_incomplete_delivery_and_criteria_cannot_complete_workspace_work() {
    for mutation in 0..4 {
        let mut store = Store::open_in_memory().unwrap();
        let (host, _) = setup(&mut store, false);
        let d = dispatch(&mut store, &host, 1);
        let mut r = result();
        match mutation {
            0 => r.deliveries.clear(),
            1 => r.criteria[0].satisfied = false,
            2 => r.criteria[0].evidence.clear(),
            _ => r.deliveries[0].reviewed_head = "branch-name".into(),
        };
        send(
            &mut store,
            &host,
            event(&d, 1, stopped(Some(r.clone()), true)),
            2,
        )
        .unwrap();
        assert_eq!(
            store.get(&d.obligation_id).unwrap().unwrap().state,
            ObligationState::Attention
        );
        assert_eq!(
            store.workspace_run(&d.execution_id).unwrap().result,
            Some(r)
        );
    }
}

#[test]
fn recurrence_uses_current_configuration_and_pause_separates_future_work() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id) = setup(&mut store, true);
    let d = dispatch(&mut store, &host, 60);
    store.managed_pause("pause", &id, 2, 61).unwrap();
    send(
        &mut store,
        &host,
        event(&d, 1, stopped(Some(result()), true)),
        62,
    )
    .unwrap();
    assert_eq!(
        store.managed_detail(&id).unwrap().status,
        ManagedTaskStatus::Paused
    );
    assert_eq!(store.managed_detail(&id).unwrap().runs.len(), 1);
    store
        .managed_resume(
            "resume",
            &id,
            3,
            &[host.projects[0].capability_profile(&host.name)],
            63,
        )
        .unwrap();
    let state = store.managed_detail(&id).unwrap();
    assert_eq!(state.runs.len(), 2);
    assert_eq!(state.next_wake_at, Some(120));
    let d = dispatch(&mut store, &host, 120);
    let mut def = store
        .managed_detail(&id)
        .unwrap()
        .active
        .unwrap()
        .definition;
    def.trigger = ManagedTrigger::Recurring {
        cron: "*/5 * * * *".into(),
        timezone: "UTC".into(),
    };
    store
        .managed_revise("edit-schedule", &id, 4, &def, 121)
        .unwrap();
    let preview = store
        .managed_preview(
            &id,
            "session",
            &[host.projects[0].capability_profile(&host.name)],
            121,
        )
        .unwrap();
    store
        .managed_activate(
            "activate-schedule",
            &preview,
            "session",
            &[host.projects[0].capability_profile(&host.name)],
            121,
        )
        .unwrap();
    send(
        &mut store,
        &host,
        event(&d, 1, stopped(Some(result()), true)),
        122,
    )
    .unwrap();
    assert_eq!(store.managed_detail(&id).unwrap().next_wake_at, Some(300));
}

#[test]
fn host_filter_precedes_the_bounded_admission_page() {
    let mut store = Store::open_in_memory().unwrap();
    let (mut other, other_id) = setup(&mut store, false);
    let mut definition = store
        .managed_detail(&other_id)
        .unwrap()
        .active
        .unwrap()
        .definition;
    let project = definition.workspace.as_ref().unwrap().project.clone();
    let mut registration = project.registration;
    registration.host = "Mac".into();
    let project = store
        .workspace_project_save(
            &ProjectSaveRequest {
                command_id: "other-host-project".into(),
                project_id: project.id,
                expected_revision: project.revision,
                registration,
            },
            0,
        )
        .unwrap();
    definition.workspace.as_mut().unwrap().project = project;
    other.name = "Mac".into();
    other.id = "mac".into();
    let profiles = [other.projects[0].capability_profile(&other.name)];
    for index in 0..100 {
        let id = store
            .managed_create(&format!("other-{index}"), &definition, 0)
            .unwrap()
            .task_id;
        let preview = store.managed_preview(&id, "session", &profiles, 0).unwrap();
        store
            .managed_activate(
                &format!("activate-other-{index}"),
                &preview,
                "session",
                &profiles,
                0,
            )
            .unwrap();
    }
    store
        .managed_pause("pause-original", &other_id, 2, 0)
        .unwrap();
    let (host, id) = setup(&mut store, false);
    let own = dispatch(&mut store, &host, 1);
    assert_eq!(own.task_id, id);
}

#[test]
fn cancelling_one_recurring_occurrence_retains_only_the_current_future_schedule() {
    for paused in [false, true] {
        let mut store = Store::open_in_memory().unwrap();
        let (host, id) = setup(&mut store, true);
        let d = dispatch(&mut store, &host, 60);
        if paused {
            store.managed_pause("pause", &id, 2, 61).unwrap();
        }
        store
            .workspace_run_action(&action(&d, 0, None, true), 62)
            .unwrap();
        assert_eq!(store.managed_detail(&id).unwrap().runs.len(), 1);
        send(&mut store, &host, event(&d, 1, stopped(None, false)), 63).unwrap();
        let state = store.managed_detail(&id).unwrap();
        assert_eq!(state.runs.len(), if paused { 1 } else { 2 });
        assert_eq!(state.next_wake_at, if paused { None } else { Some(120) });
    }
}

#[test]
fn stopped_incomplete_work_can_be_cancelled_using_its_retained_cessation_proof() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, id) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let mut retained = result();
    retained.criteria[0].satisfied = false;
    send(
        &mut store,
        &host,
        event(&d, 1, stopped(Some(retained.clone()), false)),
        2,
    )
    .unwrap();
    assert!(
        store
            .connection
            .execute(
                "UPDATE workspace_executions SET result_json='{}' WHERE execution_id=?1",
                [&d.execution_id]
            )
            .is_err()
    );
    store
        .workspace_run_action(&action(&d, 1, None, true), 3)
        .unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Cancelled
    );
    assert_eq!(
        store.workspace_run(&d.execution_id).unwrap().result,
        Some(retained)
    );
    assert!(
        store
            .workspace_exchange(&host, &empty(), 4)
            .unwrap()
            .dispatches
            .is_empty()
    );
    assert_eq!(store.managed_detail(&id).unwrap().runs.len(), 1);
}

#[test]
fn trusted_policy_rejects_a_changed_host_actions_or_execution_bounds_at_activation() {
    for mutation in 0..4 {
        let mut store = Store::open_in_memory().unwrap();
        let (host, id) = setup(&mut store, false);
        let mut definition = store
            .managed_detail(&id)
            .unwrap()
            .active
            .unwrap()
            .definition;
        match mutation {
            0 => {
                definition
                    .workspace
                    .as_mut()
                    .unwrap()
                    .project
                    .registration
                    .host = "Different host".into()
            }
            1 => definition
                .workspace
                .as_mut()
                .unwrap()
                .permitted_actions
                .push("deploy".into()),
            2 => definition.workspace.as_mut().unwrap().limits.max_seconds = 3601,
            _ => definition.profile_revision = "unregistered-profile".into(),
        }
        store
            .managed_revise("revise-policy", &id, 2, &definition, 1)
            .unwrap();
        let profiles = [host.projects[0].capability_profile(&host.name)];
        let preview = store.managed_preview(&id, "session", &profiles, 1).unwrap();
        assert!(!preview.blockers.is_empty());
        assert!(
            store
                .managed_activate("changed-policy", &preview, "session", &profiles, 1)
                .is_err()
        );
    }
}

#[test]
fn deadline_requests_cessation_without_replacing_external_execution() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    let d = dispatch(&mut store, &host, 1);
    let response = store
        .workspace_exchange(
            &host,
            &WorkspaceExchangeRequest {
                events: vec![],
                heartbeats: vec![d.execution_id.clone()],
            },
            d.deadline_at,
        )
        .unwrap();
    assert_eq!(response.dispatches.as_slice(), std::slice::from_ref(&d));
    assert!(response.controls[0].cancel);
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    assert_eq!(store.attempts(&d.obligation_id).unwrap().len(), 1);
    send(
        &mut store,
        &host,
        event(&d, 1, stopped(None, false)),
        d.deadline_at + 1,
    )
    .unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Cancelled
    );
}

#[test]
fn workspace_binding_excludes_all_generic_and_note_result_paths() {
    let mut store = Store::open_in_memory().unwrap();
    let (host, _) = setup(&mut store, false);
    assert!(store.claim_due(0, 30, 10).unwrap().is_empty());
    assert!(store.claim_due_notes(0, 30, 10).unwrap().is_empty());
    assert!(store.claim_due_reminders(0, 30, 10).unwrap().is_empty());
    let d = dispatch(&mut store, &host, 1);
    let o = store.get(&d.obligation_id).unwrap().unwrap();
    let claim = Claim {
        obligation_id: o.id.clone(),
        occurrence: o.occurrence,
        attempt_number: o.attempts_made,
        lease_token: o.lease_token.unwrap(),
        lease_generation: o.lease_generation,
        lease_expires_at: o.lease_expires_at.unwrap(),
        description: o.description,
    };
    assert!(
        store
            .complete_managed_note(&claim, "Agent said done", 2)
            .is_err()
    );
    assert!(
        store
            .complete(
                &claim,
                Completion::Succeeded {
                    evidence: Some("Process exit".into())
                },
                2
            )
            .is_err()
    );
    assert!(store.cancel(&d.obligation_id, 2).is_err());
    store.recover_expired_leases(91).unwrap();
    assert!(store.retry_attention(&d.obligation_id, 92).is_err());
    let precondition = ActionPrecondition {
        obligation_id: d.obligation_id.clone(),
        occurrence: 1,
        state_revision: store
            .events(&d.obligation_id)
            .unwrap()
            .last()
            .unwrap()
            .sequence,
        gardener_fingerprint: None,
        gardener_proposal_instance_id: None,
        gardener_source_commit: None,
        gardener_source_observation_id: None,
        gardener_source_inspection_id: None,
        gardener_generation: None,
    };
    assert!(
        store
            .retry_attention_if_current(&d.obligation_id, &precondition, 92)
            .is_err()
    );
}
