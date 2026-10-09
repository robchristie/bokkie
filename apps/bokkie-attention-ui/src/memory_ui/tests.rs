use super::*;
use bokkie_operator_api::{
    API_CONTRACT_VERSION, BOKKIE_BUILD_ID, SUPPORTED_SCHEMA_VERSION, ServiceIdentity,
    SessionBootstrap,
};

fn service() -> ServiceIdentity {
    ServiceIdentity {
        build: BOKKIE_BUILD_ID.into(),
        api_contract_version: API_CONTRACT_VERSION,
        schema_version: SUPPORTED_SCHEMA_VERSION,
        process_id: 1,
        session_id: "memory-test".into(),
    }
}
fn entry(revision: i64) -> MemoryEntry {
    MemoryEntry {
        id: "workspace:test".into(),
        revision,
        kind: MemoryKind::TaskOutcome,
        provenance: MemoryProvenance::Inferred,
        content: Some("The correction was delivered through its project workspace.".into()),
        sources: vec![MemorySource {
            reference: "workspace_execution:test".into(),
            context:
                "Accepted result for the selected task; original evidence remains with the task."
                    .into(),
        }],
        task_id: Some("task-test".into()),
        corrected: revision > 1,
        removed: false,
        created_at: 1,
        updated_at: revision,
    }
}
fn list(revision: i64) -> MemoryList {
    MemoryList {
        service: service(),
        entries: vec![entry(revision)],
        next_after: None,
    }
}

#[test]
fn correction_and_removal_pin_identity_revision_and_preserve_sources() {
    let original = entry(3);
    let mut draft = MemoryDraft::from_entry(&original);
    draft.content = "Deployment is separate from the delivered source.".into();
    let correction = draft.request(false).unwrap();
    assert_eq!(correction.expected_revision, 3);
    assert_eq!(correction.entry_id.as_deref(), Some("workspace:test"));
    assert!(matches!(
        correction.mutation,
        MemoryMutation::Correct { .. }
    ));
    assert_eq!(draft.original.as_ref().unwrap().sources, original.sources);
    assert!(draft.request(true).is_err());
    draft.confirm_remove = true;
    assert!(matches!(
        draft.request(true).unwrap().mutation,
        MemoryMutation::Remove
    ));
}

#[test]
fn session_change_bootstrap_and_current_memory_read_unlock_exact_save_and_remove_retries() {
    for remove in [false, true] {
        let context = egui::Context::default();
        Appearance::default().apply(&context);
        let mut app = super::super::tests::test_app();
        // Capture dispatches and supply adapter replies through the app's queue;
        // no HTTP request or model call is made by this regression.
        app.transport = Some(Transport::new("http://127.0.0.1:7744").unwrap());
        app.test_dispatch = Some(Vec::new());
        app.session = Some(
            ApiSession::from_bootstrap(SessionBootstrap {
                service: service(),
                mutation_token: "a".repeat(64),
            })
            .unwrap(),
        );
        app.agent_settings.open = true;
        let original = entry(1);
        let mut draft = MemoryDraft::from_entry(&original);
        draft.content = "My corrected content".into();
        draft.confirm_remove = remove;
        app.memory = MemoryState {
            open: true,
            entries: vec![original.clone()],
            draft: Some(draft),
            current: true,
            ..Default::default()
        };

        app.submit_memory(remove, &context);
        let pending = app.memory.pending.clone().unwrap();
        let save = ApiRequest::SaveMemory(pending.clone());
        assert_eq!(app.test_dispatch.as_ref().unwrap().last(), Some(&save));
        app.sender
            .send(ApiMessage {
                request: save.clone(),
                result: Err(ApiFailure::SessionChanged(
                    "Service restarted after the mutation reply was lost".into(),
                )),
            })
            .unwrap();
        app.poll_transport(&context);
        assert!(app.session.is_none());
        assert!(!app.memory.current);
        assert!(!app.memory.busy);
        assert_eq!(app.memory.pending.as_ref(), Some(&pending));
        assert_eq!(
            app.memory.draft.as_ref().unwrap().content,
            "My corrected content"
        );
        assert_eq!(
            app.test_dispatch.as_ref().unwrap().last(),
            Some(&ApiRequest::Bootstrap)
        );

        let mut new_service = service();
        new_service.session_id = "memory-restarted".into();
        new_service.process_id = 2;
        app.sender
            .send(ApiMessage {
                request: ApiRequest::Bootstrap,
                result: Ok(ApiPayload::Bootstrap(
                    ApiSession::from_bootstrap(SessionBootstrap {
                        service: new_service.clone(),
                        mutation_token: "b".repeat(64),
                    })
                    .unwrap(),
                )),
            })
            .unwrap();
        app.poll_transport(&context);
        let (generation, cursor, replace) = app
            .memory
            .reading
            .clone()
            .expect("Bootstrap must refresh the open Memory editor");
        assert_eq!(cursor, None);
        assert!(
            !replace,
            "Session recovery must preserve the draft and exact pending command"
        );
        let read = ApiRequest::Memory {
            after: None,
            generation,
        };
        assert!(app.test_dispatch.as_ref().unwrap().contains(&read));
        assert!(!app.memory.current);
        assert_eq!(app.memory.pending.as_ref(), Some(&pending));

        // A reply from the previous read/session cannot bless the editor.
        app.sender
            .send(ApiMessage {
                request: ApiRequest::Memory {
                    after: None,
                    generation: generation - 1,
                },
                result: Ok(ApiPayload::Memory(list(1))),
            })
            .unwrap();
        app.poll_transport(&context);
        assert!(!app.memory.current);
        assert_eq!(
            app.memory.reading.as_ref().map(|read| read.0),
            Some(generation)
        );
        let fresh = MemoryList {
            service: new_service.clone(),
            entries: if remove { vec![] } else { vec![entry(2)] },
            next_after: None,
        };
        app.sender
            .send(ApiMessage {
                request: read,
                result: Ok(ApiPayload::Memory(fresh)),
            })
            .unwrap();
        app.poll_transport(&context);
        assert!(app.memory.current);
        assert!(
            app.memory.conflict,
            "The read may observe an already applied change or a later revision"
        );
        assert_eq!(app.memory.pending.as_ref(), Some(&pending));
        assert_eq!(
            app.memory.draft.as_ref().unwrap().content,
            "My corrected content"
        );
        assert_eq!(
            app.memory
                .draft
                .as_ref()
                .unwrap()
                .original
                .as_ref()
                .unwrap(),
            &original
        );
        assert!(app.session.as_ref().unwrap().matches(&new_service));
        assert!(!app.session.as_ref().unwrap().matches(&service()));

        let tokens = app.theme.resolve(
            app.preferences.theme_variant(false),
            app.preferences.density_variant(),
            TypographyProfile::Reading,
        );
        let mut nodes = Vec::new();
        context
            .run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    ..Default::default()
                },
                |ui| app.show_memory(ui, tokens, &mut nodes, &mut Vec::new()),
            )
            .textures_delta
            .clear();
        assert!(
            nodes
                .iter()
                .any(|node| node.name == "Retry saved request" && node.enabled)
        );
        assert!(
            nodes
                .iter()
                .any(|node| node.name == "Reload memory" && !node.enabled)
        );

        // The footer's retry action uses the retained Save/Remove payload even
        // when the current read observed a different or removed entry.
        app.submit_memory(false, &context);
        assert_eq!(app.test_dispatch.as_ref().unwrap().last(), Some(&save));
        assert_eq!(app.memory.pending.as_ref(), Some(&pending));
        assert_eq!(pending.expected_revision, original.revision);
        app.sender
            .send(ApiMessage {
                request: save,
                result: Err(ApiFailure::Conflict(
                    "The retained command was rejected against the changed revision".into(),
                )),
            })
            .unwrap();
        app.poll_transport(&context);
        assert!(app.memory.pending.is_none());
        assert!(app.memory.conflict);
        assert_eq!(
            app.memory.draft.as_ref().unwrap().content,
            "My corrected content"
        );
        let dispatched = app.test_dispatch.as_ref().unwrap().len();
        app.submit_memory(false, &context);
        assert_eq!(app.test_dispatch.as_ref().unwrap().len(), dispatched);
    }
}

#[test]
fn creation_requires_sourced_content_without_runtime_fields() {
    let mut draft = MemoryDraft {
        kind: MemoryKind::OperationalKnowledge,
        provenance: MemoryProvenance::Inferred,
        ..MemoryDraft::default()
    };
    assert!(draft.request(false).is_err());
    draft.content = "The named workspace check passed for this revision.".into();
    draft.source_reference = "check receipt at revision abc".into();
    draft.source_context =
        "Observed successful focused verification, scoped to this revision.".into();
    let request = draft.request(false).unwrap();
    assert_eq!(request.expected_revision, 0);
    assert_eq!(request.entry_id, None);
    let json = serde_json::to_value(request).unwrap();
    assert!(json.get("permissions").is_none());
    assert_eq!(json["mutation"]["provenance"], "inferred");
    draft.source_context.clear();
    assert!(draft.request(false).is_err());
}

#[test]
fn memory_editor_exposes_sources_and_reachable_actions_at_both_widths() {
    for width in [1024.0, 358.0] {
        let context = egui::Context::default();
        Appearance::default().apply(&context);
        let mut app = super::super::tests::test_app();
        app.session = Some(
            ApiSession::from_bootstrap(SessionBootstrap {
                service: service(),
                mutation_token: "a".repeat(64),
            })
            .unwrap(),
        );
        app.memory.open = true;
        app.memory.accept(list(1), false, false);
        app.memory.draft = Some(MemoryDraft::from_entry(&entry(1)));
        let tokens = app.theme.resolve(
            app.preferences
                .theme_variant(context.theme() == egui::Theme::Dark),
            app.preferences.density_variant(),
            TypographyProfile::Reading,
        );
        let mut nodes = Vec::new();
        let mut text = Vec::new();
        for _ in 0..3 {
            nodes.clear();
            text.clear();
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 844.0),
                        )),
                        ..Default::default()
                    },
                    |ui| app.show_memory(ui, tokens, &mut nodes, &mut text),
                )
                .textures_delta
                .clear();
        }
        for label in [
            "Back to Settings",
            "Add memory",
            "Save memory",
            "Remove memory",
            "Reload memory",
        ] {
            let node = nodes
                .iter()
                .find(|n| n.name == label)
                .unwrap_or_else(|| panic!("Missing {label} at {width}"));
            assert!(
                node.rect.max_x > node.rect.min_x && node.rect.max_y > node.rect.min_y,
                "{label}"
            );
        }
        assert!(nodes.iter().any(|n| n.name == "Memory content"));
        assert!(
            nodes
                .iter()
                .any(|n| n.name.contains("workspace_execution:test"))
        );
        let issues = polyorama_ui_egui::audit_text_layouts(&text);
        assert!(issues.is_empty(), "At width {width}: {issues:?}");
    }
}
