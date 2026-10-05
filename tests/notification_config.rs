use std::{process::Command, sync::Arc};

use bokkie::{
    ManagedTaskDefinition, Store, conversation_http::ConversationConfig,
    notifications::NotificationConfig,
};
use tempfile::TempDir;

#[test]
fn invalid_notification_configuration_fails_before_database_creation() {
    let temporary = TempDir::new().unwrap();
    let database = temporary.path().join("must-not-exist.sqlite");
    let config = temporary.path().join("notification.json");
    let valid = r#"{"relay_host":"smtp-relay","relay_port":25,"from_address":"bokkie@example.org","destination":"reader@example.org","timeout_ms":1000}"#;
    for content in [
        valid.replace("1000}", "3001}"),
        valid.replace("reader@example.org", "reader@example.org,other@example.org"),
        valid.replace("1000}", "1000,\"password\":\"not-supported\"}"),
    ] {
        std::fs::write(&config, &content).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_bokkie"))
            .arg("--database")
            .arg(&database)
            .args(["serve", "--bind", "127.0.0.1:0", "--notification-config"])
            .arg(&config)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            !database.exists(),
            "invalid notification settings must precede migration"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), content);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_bokkie"))
        .arg("--database")
        .arg(&database)
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--notification-config",
            "relative.json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!database.exists());
}

#[test]
fn absent_notification_config_blocks_reminder_activation_and_valid_config_pins_the_profile_destination()
 {
    let mut store = Store::open_in_memory().unwrap();
    let task = store
        .managed_create(
            "draft",
            &ManagedTaskDefinition::reminder("Reminder", "Read queue", "reader@example.org"),
            0,
        )
        .unwrap()
        .task_id;
    let mut config = ConversationConfig {
        profile: None,
        notes_enabled: false,
        notifications: None,
        clock: None,
    };
    let blocked = store
        .managed_preview(&task, "session", &config.profiles(), 0)
        .unwrap();
    assert!(!blocked.blockers.is_empty());
    assert!(
        store
            .managed_activate("blocked", &blocked, "session", &config.profiles(), 0)
            .is_err()
    );
    let notifications = NotificationConfig {
        relay_host: "smtp-relay".into(),
        relay_port: 25,
        from_address: "bokkie@example.org".into(),
        destination: "reader@example.org".into(),
        timeout_ms: 1000,
    };
    notifications.validate().unwrap();
    config.notifications = Some(Arc::new(notifications));
    assert_eq!(config.profiles()[0].destination, "reader@example.org");
    let reviewed = store
        .managed_preview(&task, "session", &config.profiles(), 0)
        .unwrap();
    assert!(reviewed.blockers.is_empty());
    store
        .managed_activate("activate", &reviewed, "session", &config.profiles(), 0)
        .unwrap();
    // This validates source/profile wiring only; no scheduler or network is started.
    assert!(store.managed_detail(&task).unwrap().next_wake_at.is_some());
}

#[test]
fn reminder_confirmation_requires_the_complete_text_and_context_to_fit_without_truncation() {
    use bokkie::{ManagedCapabilityProfile, ManagedTaskStatus};
    let mut store = Store::open_in_memory().unwrap();
    let profiles = [ManagedCapabilityProfile::reminder("reader@example.org")];
    let reference = "r".repeat(2048);
    let context_overhead = "\n\nContext references:\n".chars().count() + reference.chars().count();
    for (number, instructions, references, fits) in [
        (1, "🦘".repeat(8192), vec![], true),
        (2, "x".repeat(8193), vec![], false),
        (
            3,
            "x".repeat(8192 - context_overhead),
            vec![reference.clone()],
            true,
        ),
        (
            4,
            "x".repeat(8193 - context_overhead),
            vec![reference],
            false,
        ),
    ] {
        let mut definition = ManagedTaskDefinition::reminder(
            "Bounded reminder",
            &instructions,
            "reader@example.org",
        );
        definition.context_refs = references.clone();
        let task = store
            .managed_create(&format!("draft-{number}"), &definition, 0)
            .unwrap()
            .task_id;
        let preview = store
            .managed_preview(&task, "session", &profiles, 0)
            .unwrap();
        let activated = store.managed_activate(
            &format!("activate-{number}"),
            &preview,
            "session",
            &profiles,
            0,
        );
        if fits {
            assert!(preview.blockers.is_empty());
            activated.unwrap();
            assert!(bokkie::managed::run_one_reminder(&mut store, 0).unwrap());
            let run = &store.managed_detail(&task).unwrap().runs[0];
            let expected = if references.is_empty() {
                instructions
            } else {
                format!(
                    "{instructions}\n\nContext references:\n{}",
                    references.join("\n")
                )
            };
            assert_eq!(expected.chars().count(), 8192);
            assert_eq!(run.result.as_deref(), Some(expected.as_str()));
            assert_eq!(run.delivery.as_ref().unwrap().body, expected);
        } else {
            assert!(
                preview
                    .blockers
                    .iter()
                    .any(|b| b.contains("complete reminder text"))
            );
            assert!(activated.is_err());
            let retained = store.managed_detail(&task).unwrap();
            assert_eq!(retained.status, ManagedTaskStatus::Draft);
            assert!(retained.runs.is_empty());
            assert!(retained.next_wake_at.is_none());
            assert_eq!(
                retained.candidate.unwrap().definition.instructions,
                instructions
            );
        }
    }
}
