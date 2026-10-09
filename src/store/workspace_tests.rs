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
    assert!(
        send(
            &mut store,
            &host,
            event(
                &d,
                1,
                WorkspaceEvent::Progress {
                    summary: "Still writing".into()
                }
            ),
            3
        )
        .is_err()
    );
    let mut invalid_stop = stopped(None, false);
    if let WorkspaceEvent::Stopped { cessation, .. } = &mut invalid_stop {
        cessation.kind = "process_exited".into();
    }
    assert!(send(&mut store, &host, event(&d, 1, invalid_stop), 4).is_err());
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Attention
    );
    let stop = event(&d, 1, stopped(Some(result()), true));
    send(&mut store, &host, stop.clone(), 4).unwrap();
    send(&mut store, &host, stop, 5).unwrap();
    assert_eq!(
        store.get(&d.obligation_id).unwrap().unwrap().state,
        ObligationState::Cancelled
    );
    assert!(
        store
            .workspace_exchange(&host, &empty(), 5)
            .unwrap()
            .dispatches
            .is_empty()
    );
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
