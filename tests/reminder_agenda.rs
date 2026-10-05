use bokkie::{ManagedCapabilityProfile, ManagedTaskDefinition, ManagedTrigger, Store};
use chrono::{TimeZone, Utc};

fn now() -> i64 {
    Utc.with_ymd_and_hms(2026, 10, 4, 22, 30, 0)
        .unwrap()
        .timestamp()
}
fn activate(store: &mut Store, command: &str, definition: ManagedTaskDefinition) -> String {
    let profiles = [ManagedCapabilityProfile::local_note()];
    let id = store
        .managed_create(command, &definition, now())
        .unwrap()
        .task_id;
    let preview = store
        .managed_preview(&id, "agenda-session", &profiles, now())
        .unwrap();
    store
        .managed_activate(
            &format!("{command}:activate"),
            &preview,
            "agenda-session",
            &profiles,
            now(),
        )
        .unwrap();
    id
}

#[test]
fn agenda_filters_before_pagination_and_retains_named_zone_and_latest_result() {
    let mut store = Store::open_in_memory().unwrap();
    for n in 0..105 {
        store
            .managed_create(
                &format!("draft-{n}"),
                &ManagedTaskDefinition::local_note("Inactive", "Not scheduled"),
                now(),
            )
            .unwrap();
    }
    let mut later = ManagedTaskDefinition::local_note("Tomorrow", "Review tomorrow's priorities");
    later.trigger = ManagedTrigger::Once {
        local_datetime: "2026-10-06T00:15".into(),
        timezone: "Australia/Adelaide".into(),
    };
    let id = activate(&mut store, "later", later);
    let upcoming = store
        .managed_catalogue_view("", None, 1, "upcoming", now(), "Australia/Adelaide")
        .unwrap();
    assert_eq!(upcoming.items.len(), 1);
    assert_eq!(upcoming.items[0].id, id);
    assert!(upcoming.next_after.is_none());
    assert_eq!(
        upcoming.items[0].summary.as_ref().unwrap().timezone,
        "Australia/Adelaide"
    );

    let immediate = activate(
        &mut store,
        "today",
        ManagedTaskDefinition::local_note("Today's result", "Review today's priorities"),
    );
    assert!(bokkie::managed::run_one_note(&mut store, now()).unwrap());
    let today = store
        .managed_catalogue_view("", None, 10, "today", now(), "Australia/Adelaide")
        .unwrap();
    assert_eq!(today.items.len(), 1);
    assert_eq!(today.items[0].id, immediate);
    assert_eq!(
        today.items[0]
            .summary
            .as_ref()
            .unwrap()
            .latest_result
            .as_deref(),
        Some("Review today's priorities")
    );
    assert_eq!(today.items[0].status, "completed");
    let input = store
        .managed_catalogue_view("", None, 20, "input", now(), "Australia/Adelaide")
        .unwrap();
    assert_eq!(input.items.len(), 20);
    assert!(input.next_after.is_some());
    assert!(
        input
            .items
            .iter()
            .all(|i| i.summary.as_ref().unwrap().needs_input)
    );
}

#[test]
fn today_uses_the_operators_calendar_and_preserves_task_specific_zone() {
    let mut store = Store::open_in_memory().unwrap();
    let mut definition =
        ManagedTaskDefinition::local_note("New York reminder", "Check the evening update");
    definition.trigger = ManagedTrigger::Once {
        local_datetime: "2026-10-05T06:00".into(),
        timezone: "America/New_York".into(),
    };
    let id = activate(&mut store, "new-york", definition);
    let page = store
        .managed_catalogue_view("", None, 20, "today", now(), "Australia/Adelaide")
        .unwrap();
    assert_eq!(page.items[0].id, id);
    assert_eq!(
        page.items[0].summary.as_ref().unwrap().timezone,
        "America/New_York"
    );
    assert!(
        store
            .managed_catalogue_view("", None, 20, "unknown", now(), "Australia/Adelaide")
            .is_err()
    );
}
