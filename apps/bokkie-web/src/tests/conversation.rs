//! A Bot's conversation and the event socket, on the Stobie host: the socket
//! is a [`Driver`] fed frames by hand, whose updates reach the application as
//! the browser transport sends them.

use bokkie_protocol::MessageId;
use bokkie_protocol::api::{
    Delivery, Message, MessagePage, MessageState, SendMessageRequest, SendMessageResponse,
};
use bokkie_protocol::events::{ClientFrame, Event, EventEnvelope, ServerFrame};

use stobie_ui_egui::ActionArgument;

use super::*;
use crate::api::SocketControl;
use crate::connection::{CLOSE_SIGNED_OUT, Input};
use crate::socket::Link;

pub(super) const TUTOR: BotId = BotId(1);

impl Fixture {
    pub(super) fn deliver(&mut self, step: Step) {
        self.io.extend(step.io);
        for update in step.updates {
            let generation = self.socket_generation;
            self.send(BokkieIntent::Socket { generation, update });
        }
    }

    pub(super) fn socket(&mut self, input: Input) {
        let step = self.driver.handle(input);
        self.deliver(step);
    }

    pub(super) fn frame(&mut self, frame: ServerFrame) {
        let step = self.driver.receive(&serde_json::to_string(&frame).unwrap());
        self.deliver(step);
    }

    pub(super) fn event(&mut self, seq: u64, event: Event) {
        self.frame(ServerFrame::Event(EventEnvelope {
            seq: Seq(seq),
            at: Timestamp(5_000 + seq as i64),
            event,
        }));
    }

    pub(super) fn io(&mut self) -> Vec<Io> {
        std::mem::take(&mut self.io)
    }

    /// The application asked for the socket: start it as the transport
    /// does, with a fresh driver.
    pub(super) fn socket_started(&mut self) {
        let generation = self.app().generation();
        assert_eq!(
            self.sent.take_socket(),
            [SocketControl::Connect(generation)]
        );
        self.socket_generation = generation;
        self.driver = Driver::new();
        self.socket(Input::Start);
        assert_eq!(self.io(), [Io::Connect]);
        self.socket(Input::Opened);
        let _ = self.io();
    }

    /// Signed in as Ada with her Tutor listed, and the socket live after
    /// event `latest`. The first live reloads the list.
    pub(super) fn signed_in_and_live(&mut self, latest: u64) {
        let _ = self.sent();
        self.sign_in();
        self.socket_started();
        self.reply(ApiReply::Bots(Ok(tutor_list(BotStatus::Idle, latest))));
        assert!(self.sent().is_empty());
        self.frame(ServerFrame::Live {
            latest: Seq(latest),
        });
        assert_eq!(self.sent(), [ApiRequest::ListBots, ApiRequest::LoadUsage]);
        self.reply(ApiReply::Bots(Ok(tutor_list(BotStatus::Idle, latest))));
        assert_eq!(self.fact("connection"), "live".into());
        assert_eq!(self.fact("bots"), "loaded".into());
    }

    /// Open the Tutor's conversation, holding `messages`, as of `seq`.
    pub(super) fn open_tutor(&mut self, messages: Vec<Message>, seq: u64) {
        self.click("Open Tutor");
        assert_eq!(self.sent(), [ApiRequest::LoadMessages(TUTOR)]);
        assert_eq!(self.fact("screen"), "conversation".into());
        assert_eq!(self.fact("conversation"), "loading".into());
        self.reply(ApiReply::Messages(
            TUTOR,
            Ok(MessagePage {
                messages,
                older: None,
                seq: Seq(seq),
            }),
        ));
        assert_eq!(self.fact("conversation"), "loaded".into());
    }

    pub(super) fn write(&mut self, draft: &str) {
        self.send(BokkieIntent::EditDraft(draft.into()));
    }
}

pub(super) fn tutor_list(status: BotStatus, seq: u64) -> BotList {
    BotList {
        bots: vec![bot(1, "Tutor", "Maths help", status, Some("Hi"))],
        seq: Seq(seq),
    }
}

pub(super) fn message(id: i64, author: Author, text: &str, state: MessageState) -> Message {
    Message {
        id: MessageId(id),
        bot_id: TUTOR,
        author,
        text: text.into(),
        state,
        created_at: Timestamp(3_000 + id),
        updated_at: Timestamp(3_000 + id),
    }
}

pub(super) fn hello() -> Vec<Message> {
    vec![
        message(1, Author::User, "Hi", MessageState::Complete),
        message(
            2,
            Author::Bot,
            "Kia ora! What shall we study?",
            MessageState::Complete,
        ),
    ]
}

pub(super) fn sent_message(id: i64, text: &str, delivery: Delivery) -> SendMessageResponse {
    SendMessageResponse {
        message: message(id, Author::User, text, MessageState::Complete),
        delivery,
    }
}

pub(super) fn delta(offset: u64, text: &str) -> Event {
    Event::MessageDelta {
        bot_id: TUTOR,
        message_id: MessageId(4),
        offset,
        delta: text.into(),
    }
}

/// The events of one exchange after event 10: the user's message 3, the
/// Bot working, its reply 4 streaming in three deltas, then complete and
/// idle. Seq 11 to 18.
pub(super) fn exchange() -> Vec<Event> {
    vec![
        Event::MessageCreated {
            message: message(3, Author::User, "Explain fractions", MessageState::Complete),
        },
        Event::BotStatusChanged {
            bot_id: TUTOR,
            status: BotStatus::Working,
        },
        Event::MessageCreated {
            message: message(4, Author::Bot, "", MessageState::Streaming),
        },
        delta(0, "A fraction "),
        delta(11, "is a part "),
        delta(21, "of a whole 🍕."),
        Event::MessageCompleted {
            message: message(
                4,
                Author::Bot,
                "A fraction is a part of a whole 🍕.",
                MessageState::Complete,
            ),
        },
        Event::BotStatusChanged {
            bot_id: TUTOR,
            status: BotStatus::Idle,
        },
    ]
}

#[test]
fn signing_in_starts_the_socket_and_reloads_the_list_once_live() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    assert!(fixture.shows("Tutor"));
    // Nothing is said about a link that works.
    assert!(!fixture.shows("Connecting…") && !fixture.shows("Reconnecting…"));
}

#[test]
fn the_socket_connecting_is_said_until_it_is_live() {
    let mut fixture = Fixture::new();
    let _ = fixture.sent();
    fixture.sign_in();
    fixture.socket_started();
    assert_eq!(fixture.fact("connection"), "connecting".into());
    fixture.reply(ApiReply::Bots(Ok(tutor_list(BotStatus::Idle, 0))));
    assert!(fixture.shows("Connecting…"));
}

#[test]
fn opening_a_bot_shows_its_conversation_and_back_returns_to_the_list() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    fixture.click("Open Tutor");
    assert!(fixture.shows("Loading the conversation…"));
    let _ = fixture.sent();
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: Some(MessageId(1)),
            seq: Seq(7),
        }),
    ));
    assert_eq!(fixture.fact("bot"), 1_i64.into());
    assert_eq!(fixture.fact("message_count"), 2_u64.into());
    for text in [
        "Tutor",
        "Maths help · Idle",
        "You",
        "Hi",
        "Kia ora! What shall we study?",
        "Older messages are not shown",
        "Message",
        "Send",
        "Back",
    ] {
        assert!(fixture.shows(text), "{text}");
    }
    assert_eq!(
        fixture.fact("reply"),
        "Kia ora! What shall we study?".into()
    );
    assert_eq!(fixture.fact("reply_state"), "complete".into());
    assert!(!fixture.enabled(BokkieAction::Send), "nothing written yet");
    fixture.click("Back");
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    assert_eq!(fixture.fact("bot"), 0_i64.into());
    assert!(fixture.store_has_no_conversation());
    assert!(fixture.shows("Open Tutor"));
}

impl Fixture {
    pub(super) fn store_has_no_conversation(&self) -> bool {
        self.app().store().conversation(TUTOR).is_none()
    }
}

#[test]
fn agents_open_a_bot_by_its_id() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(7);
    let invoke = |fixture: &mut Fixture, id: f64| {
        fixture.harness.state_mut().app_mut().perform_with(
            ActionTarget::application(BokkieAction::OpenBot),
            ActionArgument::Number(id),
            &mut (),
        )
    };
    assert_eq!(invoke(&mut fixture, 9.0), Err("There is no Bot 9".into()));
    assert_eq!(invoke(&mut fixture, 1.0), Ok(true));
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
}

#[test]
fn a_reply_streams_in_and_settles_and_the_list_follows() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    let events = exchange();
    for (seq, event) in (11..).zip(events.iter().take(4)) {
        fixture.event(seq, event.clone());
    }
    assert_eq!(fixture.fact("bot_status"), "working".into());
    assert!(fixture.shows("Maths help · Working"));
    assert!(fixture.shows("Tutor · replying…"));
    assert_eq!(fixture.fact("reply"), "A fraction ".into());
    assert_eq!(fixture.fact("reply_state"), "streaming".into());
    fixture.event(15, events[4].clone());
    fixture.event(16, events[5].clone());
    assert_eq!(
        fixture.fact("reply"),
        "A fraction is a part of a whole 🍕.".into()
    );
    assert!(fixture.shows("A fraction is a part of a whole 🍕."));
    fixture.event(17, events[6].clone());
    fixture.event(18, events[7].clone());
    assert_eq!(fixture.fact("reply_state"), "complete".into());
    assert_eq!(fixture.fact("message_count"), 4_u64.into());
    assert!(fixture.shows("Maths help · Idle"));
    assert!(fixture.sent().is_empty(), "nothing needed reloading");
    // The list's preview followed the conversation.
    fixture.click("Back");
    assert!(fixture.shows("A fraction is a part of a whole 🍕."));
}

#[test]
fn the_list_follows_events_while_no_conversation_is_open() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    for (seq, event) in (11..).zip(exchange()).take(2) {
        fixture.event(seq, event);
    }
    assert!(fixture.shows("Maths help · Working"));
    assert!(fixture.shows("Explain fractions"));
    let mut renamed = bot(1, "Maths Tutor", "Maths help", BotStatus::Working, None);
    renamed.last_message = fixture
        .app()
        .store()
        .bot(TUTOR)
        .unwrap()
        .last_message
        .clone();
    fixture.event(13, Event::BotUpdated { bot: renamed });
    assert!(fixture.shows("Open Maths Tutor"));
    fixture.event(
        14,
        Event::BotCreated {
            bot: bot(2, "Gardener", "Plans the garden", BotStatus::Idle, None),
        },
    );
    assert_eq!(fixture.fact("bot_count"), 2_u64.into());
    assert!(fixture.shows("Open Gardener"));
}

#[test]
fn sending_posts_the_draft_and_clears_it_once_delivered() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.write("  Explain fractions ");
    assert_eq!(fixture.fact("draft"), "  Explain fractions ".into());
    assert!(fixture.enabled(BokkieAction::Send));
    fixture.click("Send");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SendMessage(
            TUTOR,
            SendMessageRequest {
                text: "Explain fractions".into()
            }
        )]
    );
    assert_eq!(fixture.fact("sending"), true.into());
    assert!(fixture.shows("Sending…"));
    assert!(!fixture.enabled(BokkieAction::Send), "one at a time");
    // The field is fixed while the send is under way.
    fixture.write("changed my mind");
    assert_eq!(fixture.fact("draft"), "  Explain fractions ".into());
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Ok(sent_message(3, "Explain fractions", Delivery::NewTurn)),
    ));
    assert_eq!(fixture.fact("sending"), false.into());
    assert_eq!(fixture.fact("draft"), "".into());
    assert_eq!(fixture.fact("delivery"), "new_turn".into());
    // The message itself arrives on the socket.
    fixture.event(11, exchange()[0].clone());
    assert!(fixture.shows("Explain fractions"));
}

#[test]
fn a_message_sent_while_the_bot_replies_steers_the_reply() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    for (seq, event) in (11..).zip(exchange()).take(4) {
        fixture.event(seq, event);
    }
    fixture.write("Use pizza");
    fixture.click("Send");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Ok(sent_message(5, "Use pizza", Delivery::Steered)),
    ));
    assert_eq!(fixture.fact("delivery"), "steered".into());
    assert!(fixture.shows("Sent while the Bot was replying: it steers the reply"));
}

#[test]
fn a_failed_send_keeps_the_draft_and_says_why() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.write("Explain fractions");
    fixture.click("Send");
    let _ = fixture.sent();
    let down = "The Bots' runtime is unavailable";
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Err(error(503, ErrorCode::Unavailable, down)),
    ));
    assert_eq!(fixture.fact("draft"), "Explain fractions".into());
    assert_eq!(fixture.fact("send_error"), down.into());
    assert!(fixture.shows(down));
    assert!(fixture.enabled(BokkieAction::Send), "it can be sent again");
}

#[test]
fn typing_a_message_and_pressing_enter_sends_it() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture
        .harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, "Message")
        .click();
    fixture.harness.run();
    fixture
        .harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, "Message")
        .type_text("Explain fractions");
    fixture.harness.run();
    assert_eq!(fixture.fact("draft"), "Explain fractions".into());
    fixture.harness.key_press(egui::Key::Enter);
    fixture.harness.run();
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SendMessage(
            TUTOR,
            SendMessageRequest {
                text: "Explain fractions".into()
            }
        )]
    );
}

#[test]
fn a_bot_error_shows_until_the_next_message() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    let said = "The Bot could not finish its reply";
    fixture.event(
        11,
        Event::BotError {
            bot_id: TUTOR,
            message: said.into(),
        },
    );
    assert_eq!(fixture.fact("bot_error"), said.into());
    assert!(fixture.shows(said));
    fixture.write("Try again");
    fixture.click("Send");
    let _ = fixture.sent();
    // A send that fails keeps the error.
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Err(error(
            503,
            ErrorCode::Unavailable,
            "The Bots' runtime is unavailable",
        )),
    ));
    assert_eq!(fixture.fact("bot_error"), said.into());
    fixture.click("Send");
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Ok(sent_message(3, "Try again", Delivery::NewTurn)),
    ));
    assert_eq!(fixture.fact("bot_error"), "".into());
    assert!(!fixture.shows(said));
}

#[test]
fn a_conversation_that_failed_to_load_can_be_refreshed() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Err(error(500, ErrorCode::Internal, "Something went wrong")),
    ));
    assert!(fixture.shows("Something went wrong"));
    fixture.click("Refresh");
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
    assert!(!fixture.enabled(BokkieAction::Refresh), "already loading");
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(10),
        }),
    ));
    assert_eq!(fixture.fact("conversation"), "loaded".into());
    assert!(!fixture.shows("Refresh"), "only while it has failed");
}

#[test]
fn a_socket_dropped_mid_reply_catches_up_without_losing_or_repeating_text() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    let events = exchange();
    for (seq, event) in (11..).zip(events.iter().take(5)) {
        fixture.event(seq, event.clone());
    }
    assert_eq!(fixture.fact("reply"), "A fraction is a part ".into());
    fixture.socket(Input::Closed { code: Some(1006) });
    assert_eq!(fixture.fact("connection"), "reconnecting".into());
    assert!(fixture.shows("Reconnecting…"));
    assert!(matches!(fixture.io().as_slice(), [Io::RetryAfter(_)]));
    fixture.socket(Input::RetryDue);
    fixture.socket(Input::Opened);
    // It resumes after the last event it applied.
    assert_eq!(
        fixture.io(),
        [
            Io::Connect,
            Io::Send(crate::wire::encode_frame(&ClientFrame::Resume {
                after: Some(Seq(15))
            }))
        ]
    );
    // The replay overlaps what was applied; only the rest counts.
    for (seq, event) in (14..).zip(events.iter().skip(3)) {
        fixture.event(seq, event.clone());
        if seq == 16 {
            assert_eq!(
                fixture.fact("reply"),
                "A fraction is a part of a whole 🍕.".into()
            );
        }
    }
    fixture.frame(ServerFrame::Live { latest: Seq(18) });
    assert_eq!(fixture.fact("connection"), "live".into());
    assert!(!fixture.shows("Reconnecting…"));
    assert_eq!(fixture.fact("reply_state"), "complete".into());
    assert_eq!(fixture.fact("message_count"), 4_u64.into());
    // Resumed, not reset: nothing reloads.
    assert!(fixture.sent().is_empty());
}

#[test]
fn a_delta_beyond_the_text_held_reloads_the_conversation() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.event(
        11,
        Event::MessageCreated {
            message: message(4, Author::Bot, "", MessageState::Streaming),
        },
    );
    fixture.event(12, delta(11, "is a part "));
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
}

#[test]
fn reset_reloads_the_list_and_the_open_conversation() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.socket(Input::Closed { code: None });
    fixture.socket(Input::RetryDue);
    fixture.socket(Input::Opened);
    fixture.frame(ServerFrame::Reset { after: Seq(6_000) });
    let mut sent = fixture.sent();
    sent.sort_by_key(|request| format!("{request:?}"));
    assert_eq!(
        sent,
        [
            ApiRequest::ListBots,
            ApiRequest::LoadMessages(TUTOR),
            ApiRequest::LoadUsage
        ]
    );
    // Events after the reset are held for the snapshots loading.
    fixture.event(6_001, exchange()[0].clone());
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(6_000),
        }),
    ));
    assert_eq!(fixture.fact("message_count"), 3_u64.into());
    fixture.reply(ApiReply::Bots(Ok(tutor_list(BotStatus::Idle, 6_000))));
    fixture.frame(ServerFrame::Live { latest: Seq(6_001) });
    assert_eq!(fixture.fact("connection"), "live".into());
    assert!(fixture.sent().is_empty());
}

#[test]
fn a_reload_asked_for_while_a_load_is_in_flight_follows_it() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.click("Open Tutor");
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
    // A reset arrives before the page: the page may predate it.
    fixture.socket(Input::Closed { code: None });
    fixture.socket(Input::RetryDue);
    fixture.socket(Input::Opened);
    fixture.frame(ServerFrame::Reset { after: Seq(20) });
    assert_eq!(
        fixture.sent(),
        [ApiRequest::ListBots, ApiRequest::LoadUsage]
    );
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(10),
        }),
    ));
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
}

#[test]
fn a_visible_page_reconnects_at_once_or_checks_a_live_socket() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.socket(Input::Visible);
    assert_eq!(
        fixture.io(),
        [
            Io::Send(crate::wire::encode_frame(&ClientFrame::Ping)),
            Io::AwaitPong {
                ping: 1,
                after: crate::connection::PONG_DEADLINE
            }
        ]
    );
    // The page was hidden long enough for the socket to die quietly.
    fixture.socket(Input::PongOverdue { ping: 1 });
    assert_eq!(fixture.fact("connection"), "reconnecting".into());
    assert!(matches!(
        fixture.io().as_slice(),
        [Io::Close, Io::RetryAfter(_)]
    ));
    fixture.socket(Input::Visible);
    assert_eq!(fixture.io(), [Io::CancelRetry, Io::Connect]);
}

#[test]
fn a_failed_load_is_retried_when_the_socket_is_live_again() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.click("Open Tutor");
    let _ = fixture.sent();
    let down = "Bokkie's server is not available. Try again shortly";
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Err(error(502, ErrorCode::Unavailable, down)),
    ));
    assert_eq!(fixture.fact("conversation"), "failed".into());
    assert!(fixture.shows(down));
    fixture.socket(Input::Closed { code: None });
    fixture.socket(Input::RetryDue);
    fixture.socket(Input::Opened);
    assert!(fixture.sent().is_empty());
    fixture.frame(ServerFrame::Live { latest: Seq(10) });
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(10),
        }),
    ));
    assert_eq!(fixture.fact("conversation"), "loaded".into());
}

#[test]
fn the_gateway_closing_a_revoked_session_signs_out() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.socket(Input::Closed {
        code: Some(CLOSE_SIGNED_OUT),
    });
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(
        fixture.fact("message"),
        "Your session has ended. Sign in again".into()
    );
    assert_eq!(fixture.sent.take_socket(), [SocketControl::Disconnect]);
}

#[test]
fn a_socket_refused_before_opening_checks_the_session() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.socket(Input::Closed { code: None });
    fixture.socket(Input::RetryDue);
    fixture.socket(Input::Closed { code: Some(1006) });
    assert_eq!(fixture.sent(), [ApiRequest::CheckSession]);
    // Still valid: carry on.
    fixture.reply(ApiReply::Session(Ok(SessionResponse { user: user() })));
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    fixture.socket(Input::RetryDue);
    fixture.socket(Input::Closed { code: Some(1006) });
    let _ = fixture.sent();
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    assert_eq!(fixture.fact("screen"), "signed_out".into());
}

#[test]
fn signing_out_closes_the_socket_and_drops_its_late_updates() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.click("Sign out");
    fixture.reply(ApiReply::SignedOut(Ok(())));
    assert_eq!(fixture.sent.take_socket(), [SocketControl::Disconnect]);
    // An event already on its way when the socket closed.
    fixture.event(
        11,
        Event::BotCreated {
            bot: bot(2, "Gardener", "Plans the garden", BotStatus::Idle, None),
        },
    );
    assert_eq!(fixture.fact("screen"), "signed_out".into());
    assert_eq!(fixture.fact("bot_count"), 0_u64.into());
}

#[test]
fn an_outdated_gateway_asks_for_a_page_reload() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert!(!fixture.enabled(BokkieAction::ReloadPage));
    for _ in 0..crate::connection::UNREADABLE_LIMIT {
        let step = fixture.driver.receive(r#"{"type":"from_the_future"}"#);
        fixture.deliver(step);
        fixture.socket(Input::RetryDue);
        fixture.socket(Input::Opened);
    }
    assert_eq!(fixture.fact("connection"), "outdated".into());
    assert_eq!(fixture.app().link(), Link::Outdated);
    assert!(fixture.shows("Bokkie has been updated. Reload the page to continue"));
    fixture.click("Reload page");
    assert_eq!(fixture.sent.take_socket(), [SocketControl::ReloadPage]);
}

#[test]
fn the_conversation_presents_its_controls_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.assert_presentable();
    let mut long = hello();
    long.push(message(
        3,
        Author::Bot,
        &"A reply that runs well past eight lines at phone width. ".repeat(20),
        MessageState::Complete,
    ));
    fixture.open_tutor(long, 10);
    fixture.write("Hello");
    fixture.assert_presentable();
    assert!(fixture.shows("Send") && fixture.shows("Back"));
}
