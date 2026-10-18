//! Stopping a reply, the usage meter and the limit-reached state.

use bokkie_protocol::api::{
    Allowance, Limits, MessageState, RateLimitBucket, RateLimitWindow, StopResponse, Usage,
};
use bokkie_protocol::events::Event;

use super::conversation::{TUTOR, exchange, hello, message, sent_message};
use super::*;

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

fn usage(reached: bool, exhausted: bool) -> Usage {
    Usage {
        subscription: vec![RateLimitBucket {
            id: "codex".into(),
            name: Some("Codex".into()),
            primary: Some(RateLimitWindow {
                used_percent: if reached { 100 } else { 42 },
                window_minutes: Some(300),
                resets_at: Some(Timestamp(NOW + 2 * HOUR)),
            }),
            secondary: None,
            reached,
        }],
        allowance: Allowance {
            limits: Limits {
                daily_turns: Some(20),
                weekly_tokens: Some(100_000),
            },
            turns_today: 3,
            tokens_this_week: 812,
            exhausted,
        },
    }
}

const REACHED: &str =
    "The shared ChatGPT subscription has reached its limit. Bots can reply again in 2 h";
const EXHAUSTED: &str =
    "You have used your allowance for now. It refills over the next day or week";

impl Fixture {
    /// Live, with the Tutor's conversation open and its reply streaming.
    fn replying(&mut self) {
        self.signed_in_and_live(10);
        self.open_tutor(hello(), 10);
        for (seq, event) in (11..).zip(exchange()).take(4) {
            self.event(seq, event);
        }
        assert_eq!(self.fact("reply_state"), "streaming".into());
    }
}

#[test]
fn stop_is_offered_only_while_a_reply_runs_and_the_reply_shows_as_stopped() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    assert!(!fixture.enabled(BokkieAction::Stop));
    assert!(!fixture.shows("Stop"), "nothing to stop");
    for (seq, event) in (11..).zip(exchange()).take(4) {
        fixture.event(seq, event);
    }
    assert!(fixture.enabled(BokkieAction::Stop));
    fixture.click("Stop");
    assert_eq!(fixture.sent(), [ApiRequest::StopBot(TUTOR)]);
    assert_eq!(fixture.fact("stopping"), true.into());
    assert!(fixture.shows("Stopping…"));
    assert!(!fixture.enabled(BokkieAction::Stop), "one stop at a time");
    fixture.reply(ApiReply::Stopped(TUTOR, Ok(StopResponse { stopped: true })));
    assert_eq!(fixture.fact("stopping"), false.into());
    assert_eq!(fixture.fact("stop_note"), "".into());
    // The gateway settles the reply as interrupted and the Bot as idle.
    fixture.event(
        15,
        Event::MessageCompleted {
            message: message(4, Author::Bot, "A fraction ", MessageState::Interrupted),
        },
    );
    fixture.event(
        16,
        Event::BotStatusChanged {
            bot_id: TUTOR,
            status: BotStatus::Idle,
        },
    );
    assert_eq!(fixture.fact("reply_state"), "interrupted".into());
    assert!(fixture.shows("Tutor · stopped"));
    assert!(fixture.shows("A fraction "));
    assert!(!fixture.enabled(BokkieAction::Stop));
    assert!(!fixture.shows("Stop"));
}

#[test]
fn a_stop_that_finds_the_reply_finished_says_so() {
    let mut fixture = Fixture::new();
    fixture.replying();
    fixture.click("Stop");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Stopped(
        TUTOR,
        Ok(StopResponse { stopped: false }),
    ));
    assert_eq!(
        fixture.fact("stop_note"),
        "The reply had already finished".into()
    );
    assert!(fixture.shows("The reply had already finished"));
}

#[test]
fn a_failed_stop_says_why_and_can_be_tried_again() {
    let mut fixture = Fixture::new();
    fixture.replying();
    fixture.click("Stop");
    let _ = fixture.sent();
    let down = "The Bots' runtime is unavailable";
    fixture.reply(ApiReply::Stopped(
        TUTOR,
        Err(error(503, ErrorCode::Unavailable, down)),
    ));
    assert!(fixture.shows(down));
    assert!(fixture.enabled(BokkieAction::Stop));
}

#[test]
fn the_usage_meter_loads_and_follows_events() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live(10);
    assert_eq!(fixture.fact("usage"), "".into());
    fixture.reply(ApiReply::Usage(Ok(usage(false, false))));
    assert_eq!(fixture.fact("usage"), "loaded".into());
    assert_eq!(fixture.fact("subscription_used"), 42_i64.into());
    assert_eq!(fixture.fact("limit"), "".into());
    for text in [
        "Usage",
        "Codex: 42% of the 5-hour limit used, resets in 2 h",
        "Your turns today: 3 of 20",
        "Your tokens this week: 812 of 100,000",
    ] {
        assert!(fixture.shows(text), "{text}");
    }
    let mut later = usage(false, false);
    later.allowance.turns_today = 4;
    fixture.event(11, Event::UsageUpdated { usage: later });
    assert!(fixture.shows("Your turns today: 4 of 20"));
    assert_eq!(fixture.fact("turns_today"), 4_u64.into());
}

#[test]
fn a_meter_reply_older_than_an_event_does_not_undo_it() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    let mut newer = usage(false, false);
    newer.allowance.turns_today = 5;
    fixture.event(11, Event::UsageUpdated { usage: newer });
    // The reload's request was sent before that event arrived.
    fixture.reply(ApiReply::Usage(Ok(usage(false, false))));
    assert_eq!(fixture.fact("turns_today"), 5_u64.into());
}

#[test]
fn a_reached_subscription_shows_the_banner_on_both_screens() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Usage(Ok(usage(true, false))));
    assert_eq!(fixture.fact("limit"), REACHED.into());
    assert!(fixture.shows(REACHED));
    fixture.open_tutor(hello(), 10);
    assert!(fixture.shows(REACHED));
    // The subscription resets.
    fixture.event(
        11,
        Event::UsageUpdated {
            usage: usage(false, false),
        },
    );
    assert_eq!(fixture.fact("limit"), "".into());
    assert!(!fixture.shows(REACHED));
}

#[test]
fn a_used_up_allowance_shows_the_banner() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.event(
        11,
        Event::UsageUpdated {
            usage: usage(false, true),
        },
    );
    assert_eq!(fixture.fact("limit"), EXHAUSTED.into());
    assert!(fixture.shows(EXHAUSTED));
}

#[test]
fn a_send_refused_with_limit_reached_keeps_the_draft() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.write("One more question");
    fixture.click("Send");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Err(error(429, ErrorCode::LimitReached, EXHAUSTED)),
    ));
    assert_eq!(fixture.fact("draft"), "One more question".into());
    assert_eq!(fixture.fact("send_error"), EXHAUSTED.into());
    assert!(fixture.shows(EXHAUSTED));
    assert!(fixture.enabled(BokkieAction::Send), "it can be sent later");
    // The meter's own event follows, and the banner shows too.
    fixture.event(
        11,
        Event::UsageUpdated {
            usage: usage(false, true),
        },
    );
    assert_eq!(fixture.fact("limit"), EXHAUSTED.into());
    // Once it refills, the same draft goes.
    fixture.event(
        12,
        Event::UsageUpdated {
            usage: usage(false, false),
        },
    );
    fixture.click("Send");
    assert_eq!(
        fixture.sent(),
        [ApiRequest::SendMessage(
            TUTOR,
            bokkie_protocol::api::SendMessageRequest {
                text: "One more question".into()
            }
        )]
    );
    fixture.reply(ApiReply::Sent(
        TUTOR,
        Ok(sent_message(
            3,
            "One more question",
            bokkie_protocol::api::Delivery::NewTurn,
        )),
    ));
    assert_eq!(fixture.fact("draft"), "".into());
}

#[test]
fn the_limit_and_stop_controls_fit_at_phone_width() {
    let mut fixture = Fixture::new();
    fixture.sent.set_now(Timestamp(NOW));
    fixture.signed_in_and_live(10);
    fixture.reply(ApiReply::Usage(Ok(usage(true, true))));
    fixture.assert_presentable();
    fixture.replying_from_open();
    fixture.assert_presentable();
    assert!(fixture.shows("Stop") && fixture.shows("Send"));
}

impl Fixture {
    fn replying_from_open(&mut self) {
        self.open_tutor(hello(), 10);
        for (seq, event) in (11..).zip(exchange()).take(4) {
            self.event(seq, event);
        }
    }
}
