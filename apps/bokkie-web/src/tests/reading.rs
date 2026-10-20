//! How a conversation reads: the user's messages as written, the Bot's
//! replies as Markdown whose links open a new tab, and the gateway's markers
//! as compact lines. Rows' controls carry their row's ID to agents.

use bokkie_protocol::api::{
    Message, MessageKind, MessageNotification, MessageRoutine, MessageState, NotificationKind,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{MessageId, RoutineId, RoutineRunId};
use stobie_ui_egui::{
    ActionKey, CapabilityDescription, InspectionReply, InspectionSelector, InvocationReceipt,
    ReceiptState, UiNode, UiRole,
};

use egui_kittest::kittest::by;

use super::conversation::{TUTOR, hello, message, tutor_list};
use super::*;

const REPLY: &str = "Read **this** at [Example](https://example.org/).";

impl Fixture {
    /// The page's semantic node named `name`, as Stobie publishes it.
    fn node(&self, name: &str) -> UiNode {
        self.host()
            .ui_snapshot()
            .nodes
            .iter()
            .find(|node| node.name == name)
            .unwrap_or_else(|| panic!("no node named {name:?}"))
            .clone()
    }

    fn inspect(&self, id: &str, operation: InspectionOperation) -> InspectionReply {
        let inspection = self.host().inspection().unwrap();
        inspection.handle(InspectionRequest {
            version: 1,
            request_id: id.into(),
            instance: Some(inspection.instance()),
            operation,
        })
    }

    /// Every binding of `action` an agent discovers.
    fn discover(&self, action: BokkieAction) -> Vec<CapabilityDescription> {
        let reply = self.inspect(
            "discover",
            InspectionOperation::Discover {
                selector: InspectionSelector {
                    capability: Some(action.stable_id().into()),
                    ..Default::default()
                },
                limit: 64,
                cursor: None,
            },
        );
        let Some(InspectionResult::Discover { capabilities, .. }) = reply.result else {
            panic!("discovery failed: {:?}", reply.error);
        };
        capabilities
    }

    /// Invoke `binding` with `value` as an agent does, run passes until the
    /// invocation has drained, and read its receipt.
    fn invoke(
        &mut self,
        id: &str,
        binding: &CapabilityDescription,
        value: f64,
    ) -> InvocationReceipt {
        let admitted = receipt(self.inspect(
            id,
            InspectionOperation::Invoke {
                selector: InspectionSelector {
                    id: Some(binding.id.clone()),
                    ..Default::default()
                },
                expected: binding.target.clone(),
                arguments: serde_json::json!({ "value": value }),
            },
        ));
        if admitted.state != ReceiptState::Queued {
            return admitted;
        }
        self.harness.run();
        receipt(self.inspect(
            &format!("lookup-{id}"),
            InspectionOperation::Receipt {
                request_id: id.into(),
            },
        ))
    }
}

fn receipt(reply: InspectionReply) -> InvocationReceipt {
    let Some(InspectionResult::Receipt { receipt }) = reply.result else {
        panic!("receipt failed: {:?}", reply.error);
    };
    receipt
}

fn marker(id: i64, title: &str) -> Message {
    Message {
        kind: MessageKind::Marker,
        ..message(id, Author::Bot, title, MessageState::Complete)
    }
}

/// A routine run's marker, titled as the gateway titles it: the routine's
/// name and when the run was due, in the user's zone.
fn routine_run(id: i64, title: &str, prompt: &str) -> Message {
    Message {
        routine: Some(MessageRoutine {
            routine_id: RoutineId(1),
            run_id: RoutineRunId(id),
            prompt: prompt.into(),
        }),
        ..marker(id, title)
    }
}

fn notification(id: i64, kind: NotificationKind, title: &str, body: &str) -> Message {
    Message {
        notification: Some(MessageNotification {
            kind,
            body: body.into(),
        }),
        ..marker(id, title)
    }
}

#[test]
fn a_bots_reply_reads_as_markdown_and_its_links_open_a_new_tab() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    let mut messages = hello();
    messages.push(message(3, Author::User, REPLY, MessageState::Complete));
    messages.push(message(4, Author::Bot, REPLY, MessageState::Complete));
    fixture.open_tutor(messages, 7);
    // The user's message is shown as they wrote it.
    assert!(fixture.shows(REPLY));
    // The Bot's reply is formatted, with its link a link of its own.
    assert!(fixture.shows("Read this at Example."));
    let link = fixture.node("Example");
    assert_eq!(link.role, UiRole::Link);
    assert_eq!(link.value.as_deref(), Some("https://example.org/"));
    assert_eq!(link.description.as_deref(), Some("Opens in a new tab"));
    // Assistive technology meets it as a link too.
    assert!(
        fixture
            .harness
            .query(by().role(egui::accesskit::Role::Link).label("Example"))
            .is_some()
    );
}

#[test]
fn a_reply_streaming_in_reads_as_markdown_as_it_grows() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    let mut messages = hello();
    messages.push(message(
        4,
        Author::Bot,
        "Here is **half [a li",
        MessageState::Streaming,
    ));
    fixture.open_tutor(messages, 7);
    // Markdown cut off mid-way reads as the text so far, unformatted.
    assert!(fixture.shows("Tutor · 9:30 AM · replying…"));
    assert!(fixture.shows("Here is **half [a li"));
    fixture.event(
        8,
        Event::MessageDelta {
            bot_id: TUTOR,
            message_id: MessageId(4),
            offset: 20,
            delta: "nk](https://example.org/)**.".into(),
        },
    );
    assert!(fixture.shows("Here is half a link."));
    assert_eq!(fixture.node("a link").role, UiRole::Link);
}

#[test]
fn a_long_reply_is_shown_in_full() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    let lines: Vec<String> = (1..=14).map(|line| format!("Line {line}")).collect();
    let mut messages = hello();
    messages.push(message(
        3,
        Author::Bot,
        &lines.join("\n\n"),
        MessageState::Complete,
    ));
    fixture.open_tutor(messages, 7);
    for line in &lines {
        fixture.reveal(line);
        assert!(fixture.shows(line), "{line} is shown");
    }
}

#[test]
fn markers_read_as_compact_lines_with_their_time() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    fixture.open_tutor(
        vec![
            marker(1, "Capabilities changed"),
            notification(
                2,
                NotificationKind::Question,
                "Which day suits?",
                "Tuesday or Thursday.",
            ),
            notification(3, NotificationKind::Result, "Revision plan ready", ""),
        ],
        7,
    );
    assert!(fixture.shows("Capabilities changed · 9:30 AM"));
    assert!(fixture.shows("Question: Which day suits? · 9:30 AM"));
    assert!(fixture.shows("Tuesday or Thursday."));
    assert!(fixture.shows("Result: Revision plan ready · 9:30 AM"));
    // A marker is the gateway's, not something the Bot said.
    assert!(!fixture.shows("Tutor · 9:30 AM"));
}

#[test]
fn a_routine_runs_marker_folds_away_its_prompt() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    let prompt = "Ask me three questions on fractions.";
    fixture.open_tutor(
        vec![
            routine_run(1, "Morning quiz · 7:00", prompt),
            marker(2, "Capabilities changed"),
            routine_run(
                3,
                "Evening quiz · 19:00 · test run",
                "Ask me about decimals.",
            ),
        ],
        7,
    );
    // The gateway's title says when the run was due; no second time.
    assert!(fixture.shows("Routine run: Morning quiz · 7:00"));
    assert!(!fixture.shows(prompt));
    // Each run's control is its own.
    let snapshot = fixture.host().ui_snapshot();
    assert!(
        snapshot.semantic_audit.is_empty(),
        "{:?}",
        snapshot.semantic_audit
    );
    assert_eq!(
        snapshot
            .by_action(BokkieAction::ShowPrompt)
            .map(|node| (node.name.as_str(), node.argument.clone()))
            .collect::<Vec<_>>(),
        [
            (
                "Show prompt Morning quiz · 7:00",
                Some(serde_json::json!(1))
            ),
            (
                "Show prompt Evening quiz · 19:00 · test run",
                Some(serde_json::json!(3))
            ),
        ]
    );
    assert_eq!(
        fixture.node("Show prompt Morning quiz · 7:00").checked,
        Some(false)
    );

    fixture.click("Show prompt Morning quiz · 7:00");
    assert!(fixture.shows(prompt));
    assert!(!fixture.shows("Ask me about decimals."));
    assert_eq!(
        fixture.node("Show prompt Morning quiz · 7:00").checked,
        Some(true)
    );

    // An agent folds it again through the marker's own control.
    let bindings = fixture.discover(BokkieAction::ShowPrompt);
    let row = bindings
        .iter()
        .find(|binding| binding.argument == Some(serde_json::json!(1)))
        .expect("the marker's control carries its message ID");
    assert_eq!(
        fixture.invoke("fold", row, 1.0).state,
        ReceiptState::Completed
    );
    assert!(!fixture.shows(prompt));

    // A marker that is no routine run has no prompt to show.
    let own = bindings
        .iter()
        .find(|binding| binding.argument.is_none())
        .expect("the action's own binding");
    assert_eq!(
        fixture.invoke("not-a-run", own, 2.0).state,
        ReceiptState::Rejected
    );
}

#[test]
fn agents_find_each_bots_row_and_only_bots_that_are_listed() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    let bindings = fixture.discover(BokkieAction::OpenBot);
    let rows: Vec<_> = bindings
        .iter()
        .filter_map(|binding| binding.argument.clone())
        .collect();
    assert_eq!(rows, [serde_json::json!(1)], "one binding per row");
    assert_eq!(
        fixture.row("Tutor").expect("the row").argument,
        Some(serde_json::json!(1))
    );

    // A Bot that is not listed cannot be opened, whatever ID is sent.
    let own = bindings
        .iter()
        .find(|binding| binding.argument.is_none())
        .expect("the action's own binding")
        .clone();
    assert_eq!(
        fixture.invoke("absent", &own, 9.0).state,
        ReceiptState::Rejected
    );
    assert_eq!(fixture.sent(), []);

    let row = bindings
        .iter()
        .find(|binding| binding.argument.is_some())
        .unwrap()
        .clone();
    assert_eq!(
        fixture.invoke("open", &row, 1.0).state,
        ReceiptState::Completed
    );
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
}

#[test]
fn a_bot_that_leaves_the_list_loses_its_row() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    assert!(fixture.lists("Tutor"));
    fixture.click("Refresh Bots");
    fixture.reply(ApiReply::Bots(Ok(BotList {
        bots: Vec::new(),
        ..tutor_list(BotStatus::Idle, 8)
    })));
    assert!(!fixture.lists("Tutor"));
    assert!(
        fixture
            .discover(BokkieAction::OpenBot)
            .iter()
            .all(|binding| binding.argument.is_none())
    );
}
