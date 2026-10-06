use super::*;
use crate::conversation::ConversationOperation;

fn service() -> ServiceIdentity {
    ServiceIdentity {
        build: BOKKIE_BUILD_ID.into(),
        api_contract_version: API_CONTRACT_VERSION,
        schema_version: SUPPORTED_SCHEMA_VERSION,
        process_id: 1,
        session_id: "synthetic".into(),
    }
}
fn registration(host: &str) -> ProjectRegistration {
    ProjectRegistration {
        name: "Atlas".into(),
        host: host.into(),
        workspace: "/development/atlas".into(),
        codex_project_id: Some(Uuid::new_v4().to_string()),
        codex_host_id: Some(format!("remote-ssh-codex-managed:{host}")),
        context: "Project list and navigation".into(),
    }
}
fn register(s: &mut Store, host: &str) -> ProjectDestination {
    s.workspace_project_save(
        &ProjectSaveRequest {
            command_id: Uuid::new_v4().to_string(),
            project_id: Uuid::new_v4().to_string(),
            expected_revision: 0,
            registration: registration(host),
        },
        100,
    )
    .unwrap()
}
fn brief() -> HandoffBrief {
    HandoffBrief {
        outcome: "Add a searchable project list".into(),
        context: "Retain project selection".into(),
        constraints: "No deployment or publication".into(),
        acceptance: "Filtering and clearing retain the selected project".into(),
        references: vec!["https://example.org/reference".into()],
    }
}
fn prepare(s: &mut Store, query: &str) -> HandoffDraft {
    let req = ConversationTurnRequest {
        command_id: Uuid::new_v4().to_string(),
        conversation_id: Uuid::new_v4().to_string(),
        expected_revision: 0,
        text: "Prepare a hand-off for Atlas".into(),
        consult_adviser: false,
    };
    s.conversation_begin(&req, "synthetic", 100).unwrap();
    s.handoff_prepare(&req, query, &brief(), 100).unwrap();
    s.handoff_prepare(&req, query, &brief(), 101).unwrap();
    s.conversation_finish(&req, "Review the draft", None, 101)
        .unwrap();
    s.conversation_handoff(&req.conversation_id)
        .unwrap()
        .unwrap()
}
fn save_request(d: &HandoffDraft, p: &ProjectDestination) -> HandoffSaveRequest {
    HandoffSaveRequest {
        command_id: Uuid::new_v4().to_string(),
        draft_id: d.id.clone(),
        expected_revision: 0,
        project_id: p.id.clone(),
        project_revision: p.revision,
        brief: d.brief.clone(),
    }
}
#[test]
fn handoff_ambiguity_revisions_return_and_results_survive_restart_without_execution() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("synthetic.sqlite");
    let mut s = Store::open(&db).unwrap();
    let p = register(&mut s, "lv426");
    register(&mut s, "another-host");
    let draft = prepare(&mut s, "Atlas");
    assert_eq!(draft.candidates.len(), 2);
    assert_eq!(draft.saved_revision, 0);
    let mut req = save_request(&draft, &p);
    let old = s
        .handoff_save(&req, "https://bokkie.example.org", 110)
        .unwrap();
    assert!(
        old.complete_brief
            .contains(&format!("https://bokkie.example.org{}", old.return_path))
    );
    assert!(
        old.complete_brief
            .contains("Read this workspace's AGENTS.md")
    );
    assert!(
        old.complete_brief
            .contains("Execution has not been acknowledged")
    );
    req.command_id = Uuid::new_v4().to_string();
    req.expected_revision = 1;
    req.brief.acceptance.push_str(". Keyboard input works");
    let new = s
        .handoff_save(&req, "https://bokkie.example.org", 120)
        .unwrap();
    assert_eq!(new.revision, 2);
    let current = s
        .conversation_handoff(&draft.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.saved_revision, 2);
    assert_eq!(current.brief, new.brief);
    for kind in [
        HandoffActivityKind::CopyRequested,
        HandoffActivityKind::CopySucceeded,
        HandoffActivityKind::CopyFailed,
        HandoffActivityKind::ManualOpeningViewed,
        HandoffActivityKind::OpeningProblemReported,
        HandoffActivityKind::ResultNote,
    ] {
        let action = HandoffActivityRequest {
            command_id: Uuid::new_v4().to_string(),
            handoff_id: old.id.clone(),
            revision: 1,
            kind,
            note: "Synthetic operator report".into(),
        };
        s.handoff_activity(&action, 130).unwrap();
        s.handoff_activity(&action, 131).unwrap();
    }
    assert!(s.list().unwrap().is_empty());
    assert_eq!(s.conversation_model_dispatch_count().unwrap(), 0);
    drop(s);
    let s = Store::open(&db).unwrap();
    let restored = s.handoff_view(&old.id, Some(1), service()).unwrap();
    assert_eq!(restored.snapshot, old);
    assert_eq!(restored.latest_revision, 2);
    assert_eq!(restored.activities.len(), 6);
    assert!(
        restored
            .activities
            .last()
            .unwrap()
            .provenance
            .contains("not independently verified")
    );
    assert_eq!(
        s.handoff_view(&new.id, None, service()).unwrap().snapshot,
        new
    );
    assert_eq!(s.handoff_list(None, service()).unwrap().items.len(), 1);
    assert!(s.list().unwrap().is_empty());
    assert_eq!(s.conversation_model_dispatch_count().unwrap(), 0);
}
#[test]
fn handoff_saves_are_replay_safe_and_identical_clicks_do_not_duplicate() {
    let mut s = Store::open_in_memory().unwrap();
    let p = register(&mut s, "lv426");
    let d = prepare(&mut s, "Atlas");
    let mut req = save_request(&d, &p);
    let saved = s.handoff_save(&req, "http://127.0.0.1:7744", 100).unwrap();
    assert_eq!(
        s.handoff_save(&req, "http://127.0.0.1:7744", 200).unwrap(),
        saved
    );
    req.brief.outcome.push_str(" and details");
    assert!(matches!(
        s.handoff_save(&req, "http://127.0.0.1:7744", 200),
        Err(StoreError::Conflict(_))
    ));
    req.brief = saved.brief.clone();
    req.command_id = Uuid::new_v4().to_string();
    assert_eq!(
        s.handoff_save(&req, "http://127.0.0.1:7744", 300).unwrap(),
        saved
    );
    req.brief.outcome.push_str(" and details");
    req.command_id = Uuid::new_v4().to_string();
    assert!(matches!(
        s.handoff_save(&req, "http://127.0.0.1:7744", 400),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(latest(&s.connection, &d.id).unwrap(), 1);
}
#[test]
fn project_edits_fence_selection_and_never_rewrite_historical_destinations() {
    let mut s = Store::open_in_memory().unwrap();
    let p = register(&mut s, "lv426");
    let d = prepare(&mut s, "Atlas");
    let mut req = save_request(&d, &p);
    let saved = s
        .handoff_save(&req, "https://bokkie.example.org", 100)
        .unwrap();
    let mut update = ProjectSaveRequest {
        command_id: Uuid::new_v4().to_string(),
        project_id: p.id.clone(),
        expected_revision: 1,
        registration: p.registration.clone(),
    };
    update.registration.workspace = "/development/atlas-new".into();
    let changed = s.workspace_project_save(&update, 200).unwrap();
    assert_eq!(s.workspace_project_save(&update, 201).unwrap(), changed);
    update.registration.host = "different-host".into();
    assert!(matches!(
        s.workspace_project_save(&update, 201),
        Err(StoreError::Conflict(_))
    ));
    update.command_id = Uuid::new_v4().to_string();
    assert!(matches!(
        s.workspace_project_save(&update, 201),
        Err(StoreError::Conflict(_))
    ));
    req.command_id = Uuid::new_v4().to_string();
    req.expected_revision = 1;
    assert!(matches!(
        s.handoff_save(&req, "https://bokkie.example.org", 210),
        Err(StoreError::Conflict(_))
    ));
    req.project_revision = 2;
    let revised = s
        .handoff_save(&req, "https://bokkie.example.org", 220)
        .unwrap();
    assert_eq!(revised.revision, 2);
    assert_eq!(
        revised.project.registration.workspace,
        "/development/atlas-new"
    );
    assert_eq!(
        s.handoff_view(&d.id, Some(1), service()).unwrap().snapshot,
        saved
    );
}
#[test]
fn invalid_and_missing_destinations_cannot_be_saved_or_interpreted_as_launch_routes() {
    let mut s = Store::open_in_memory().unwrap();
    let d = prepare(&mut s, "Missing");
    assert!(d.candidates.is_empty());
    let mut req = save_request(
        &d,
        &ProjectDestination {
            id: Uuid::new_v4().to_string(),
            revision: 1,
            registration: registration("lv426"),
        },
    );
    assert!(matches!(
        s.handoff_save(&req, "https://bokkie.example.org", 100),
        Err(StoreError::NotFound(_))
    ));
    req.project_id = "codex://project/example".into();
    assert!(
        s.handoff_save(&req, "https://bokkie.example.org", 100)
            .is_err()
    );
    for path in [
        "relative",
        "https://workspace.example.org",
        "codex://project/id",
        "file:///tmp/project",
        "/development/../private",
        "/tmp/project\ncommand",
    ] {
        let mut p = registration("lv426");
        p.workspace = path.into();
        assert!(validate_registration(&p).is_err(), "{path}");
    }
    let mut p = registration("another-host");
    p.workspace = "C:\\Development\\Atlas".into();
    assert!(validate_registration(&p).is_ok());
    p.codex_project_id = Some("invented".into());
    assert!(validate_registration(&p).is_err());
    p.codex_project_id = None;
    assert!(validate_registration(&p).is_err());
    p.codex_host_id = None;
    assert!(validate_registration(&p).is_ok());
    let mut value = serde_json::to_value(&p).unwrap();
    value["opening_url"] = serde_json::json!("https://workspace.example.org/run");
    assert!(serde_json::from_value::<ProjectRegistration>(value).is_err());
}
#[test]
fn invalid_generated_briefs_are_rejected_before_persistence() {
    for link in [
        "javascript:alert(1)",
        "codex://project/a",
        "file:///tmp/secret",
        "https://user:secret@example.org/path",
        "not a link",
    ] {
        let mut b = brief();
        b.references = vec![link.into()];
        assert!(validate_brief(&b).is_err(), "{link}");
    }
    let mut b = brief();
    b.acceptance = " ".into();
    assert!(validate_brief(&b).is_err());
    b = brief();
    b.context = "x".repeat(4097);
    assert!(validate_brief(&b).is_err());
    let output = serde_json::json!({"tool":"bokkie_prepare_handoff","arguments":{"project_query":"Atlas","brief":brief(),"shell":"run work"}});
    assert!(
        crate::conversation_tools::operation(
            output,
            &crate::conversation_tools::tools(false, false),
            None
        )
        .is_err()
    );
    let output = serde_json::json!({"tool":"bokkie_prepare_handoff","arguments":{"project_query":"Atlas","brief":brief()}});
    assert!(matches!(
        crate::conversation_tools::operation(
            output,
            &crate::conversation_tools::tools(false, false),
            None
        )
        .unwrap(),
        ConversationOperation::PrepareHandoff { .. }
    ));
}
#[test]
fn handoff_writes_and_receipts_roll_back_together_when_audit_fails() {
    let mut s = Store::open_in_memory().unwrap();
    let p = register(&mut s, "lv426");
    let d = prepare(&mut s, "Atlas");
    let req = save_request(&d, &p);
    s.connection.execute_batch("CREATE TRIGGER fail_handoff_event BEFORE INSERT ON domain_events WHEN NEW.event_type='workspace_handoff_changed' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(
        s.handoff_save(&req, "https://bokkie.example.org", 100)
            .is_err()
    );
    assert_eq!(latest(&s.connection, &d.id).unwrap(), 0);
    assert_eq!(
        s.connection
            .query_row(
                "SELECT COUNT(*) FROM workspace_handoff_commands WHERE command_id=?1",
                [&req.command_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    s.connection
        .execute_batch("DROP TRIGGER fail_handoff_event")
        .unwrap();
    let saved = s
        .handoff_save(&req, "https://bokkie.example.org", 100)
        .unwrap();
    for sql in [
        "UPDATE workspace_handoff_snapshots SET revision=2",
        "DELETE FROM workspace_handoff_snapshots",
        "DELETE FROM workspace_handoff_commands",
        "UPDATE workspace_handoff_drafts SET project_query='changed'",
    ] {
        assert!(s.connection.execute(sql, []).is_err());
    }
    let action = HandoffActivityRequest {
        command_id: Uuid::new_v4().to_string(),
        handoff_id: saved.id,
        revision: 1,
        kind: HandoffActivityKind::ResultNote,
        note: "Operator says this is complete; no external verification".into(),
    };
    s.handoff_activity(&action, 110).unwrap();
    let mut changed = action.clone();
    changed.note = "Changed".into();
    assert!(matches!(
        s.handoff_activity(&changed, 120),
        Err(StoreError::Conflict(_))
    ));
    assert!(
        s.connection
            .execute("DELETE FROM workspace_handoff_activities", [])
            .is_err()
    );
}
