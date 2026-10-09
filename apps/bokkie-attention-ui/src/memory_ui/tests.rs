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
fn uncertain_result_retries_exact_command_and_conflict_retains_operator_text() {
    let mut state = MemoryState::default();
    state.accept(list(1), false, false);
    let mut draft = MemoryDraft::from_entry(&entry(1));
    draft.content = "My corrected content".into();
    state.draft = Some(draft);
    let pending = state.command(false).unwrap();
    state.failed(&ApiFailure::Other("Lost reply".into()));
    state.reset_session();
    state.accept(list(2), false, false);
    assert_eq!(state.command(false).unwrap(), pending);
    state.failed(&ApiFailure::Conflict("Revision changed".into()));
    assert!(state.command(false).is_err());
    assert_eq!(
        state.draft.as_ref().unwrap().content,
        "My corrected content"
    );
    state.accept(list(2), false, true);
    assert!(!state.conflict);
    assert!(state.draft.is_none());
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
