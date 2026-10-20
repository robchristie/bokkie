//! The device's notifications switch, the read markers and `viewing`, the
//! unread state and the app badge, and a tapped notification's route, with
//! Web Push's results injected as the bridge delivers them.

use bokkie_protocol::MessageId;
use bokkie_protocol::api::{
    MessagePage, MessageState, PushKey, PushSubscriptionKeys, PushSubscriptionRequest,
    PushSubscriptionState, ReadState,
};
use bokkie_protocol::events::Event;
use stobie_ui_egui::Availability;
use stobie_ui_egui::web_push::{
    NotificationPermission, NotificationRoute, PushEvent, PushOperation, PushState,
    PushSubscription, PushSupport,
};

use super::conversation::{TUTOR, hello, message, tutor_list};
use super::*;
use crate::api::DeviceControl;

const ENDPOINT: &str = "https://push.example/device-1";

fn subscription() -> PushSubscription {
    PushSubscription {
        endpoint: ENDPOINT.into(),
        json: format!(
            r#"{{"endpoint":"{ENDPOINT}","expirationTime":null,"keys":{{"p256dh":"BPUB","auth":"AUTH"}}}}"#
        ),
    }
}

fn request() -> ApiRequest {
    ApiRequest::PutPushSubscription(PushSubscriptionRequest {
        endpoint: ENDPOINT.into(),
        keys: PushSubscriptionKeys {
            p256dh: "BPUB".into(),
            auth: "AUTH".into(),
        },
    })
}

fn state(permission: NotificationPermission, subscribed: bool) -> PushEvent {
    PushEvent::State(PushState {
        support: PushSupport::Supported,
        permission: Some(permission),
        subscription: subscribed.then(subscription),
    })
}

fn key(sending: bool) -> ApiReply {
    ApiReply::PushKey(Ok(PushKey {
        public_key: "SERVER-KEY".into(),
        sending,
    }))
}

fn route(path: &str) -> PushEvent {
    PushEvent::Route(NotificationRoute {
        url: format!("https://bokkie.example/{path}"),
        path: path.into(),
    })
}

fn read(read_up_to: i64, unread: bool) -> Event {
    Event::ReadUpdated {
        read: ReadState {
            bot_id: TUTOR,
            read_up_to: Some(MessageId(read_up_to)),
            unread,
        },
    }
}

fn availability(fixture: &Fixture) -> Availability {
    fixture
        .app()
        .availability(ActionTarget::application(BokkieAction::Notifications))
}

impl Fixture {
    fn push(&mut self, event: PushEvent) {
        self.send(BokkieIntent::Push(event));
    }

    /// Signed in and live, on a device whose notifications are off, with
    /// the gateway's key loaded.
    fn notifications_off(&mut self) {
        self.signed_in_and_live(10);
        self.push(state(NotificationPermission::Default, false));
        assert_eq!(self.sent(), [ApiRequest::PushKey]);
        assert_eq!(
            availability(self),
            Availability::Disabled {
                reason: "Loading Bokkie's push key".into()
            }
        );
        self.reply(key(true));
        assert_eq!(self.fact("push"), "off".into());
        assert!(self.enabled(BokkieAction::Notifications));
    }

    /// As [`Self::notifications_off`], then switched on and saved.
    fn notifications_on(&mut self) {
        self.notifications_off();
        self.reveal("Notifications on this device");
        self.click("Notifications on this device");
        assert_eq!(
            self.push_controls(),
            [DeviceControl::Subscribe("SERVER-KEY".into())]
        );
        assert_eq!(self.fact("push"), "subscribing".into());
        assert!(self.sent().is_empty(), "nothing is sent before the browser");
        self.push(PushEvent::Subscribed(subscription()));
        assert_eq!(self.sent(), [request()]);
        assert_eq!(self.fact("push"), "on".into());
        assert_eq!(self.fact("push_saved"), false.into());
        self.reply(ApiReply::PushSubscribed(Ok(PushSubscriptionState {
            subscribed: true,
        })));
        assert_eq!(self.fact("push_saved"), true.into());
    }

    /// What was asked of Web Push, the badge aside.
    fn push_controls(&self) -> Vec<DeviceControl> {
        let mut controls = self.sent.take_device();
        controls.retain(|control| !matches!(control, DeviceControl::Badge(_)));
        controls
    }

    /// Every fact, as one string.
    fn all_facts(&self) -> String {
        let inspection = self.host().inspection().unwrap();
        let reply = inspection.handle(InspectionRequest {
            version: 1,
            request_id: "observe".into(),
            instance: Some(inspection.instance()),
            operation: InspectionOperation::Observe,
        });
        let Some(InspectionResult::Observe { observation }) = reply.result else {
            panic!("observe failed");
        };
        format!("{:?}", observation.facts)
    }
}

#[test]
fn the_switch_subscribes_from_the_tap_and_sends_the_subscription_once() {
    let mut fixture = Fixture::new();
    fixture.notifications_on();
    assert_eq!(fixture.fact("push_sending"), true.into());
    // Later passes and a repeated state send nothing more.
    fixture.push(state(NotificationPermission::Granted, true));
    fixture.event(11, read(1, false));
    assert!(fixture.sent().is_empty());
    assert!(fixture.push_controls().is_empty());
}

#[test]
fn the_endpoint_never_reaches_a_fact_or_a_log_line() {
    let mut fixture = Fixture::new();
    fixture.notifications_on();
    assert!(!fixture.all_facts().contains("push.example"));
    let logged = format!(
        "{:?} {:?} {:?} {:?}",
        BokkieIntent::Push(PushEvent::Subscribed(subscription())),
        BokkieIntent::Push(state(NotificationPermission::Granted, true)),
        request(),
        fixture.app().device,
    );
    assert!(!logged.contains("push.example"), "{logged}");
    assert!(
        !logged.contains("BPUB") && !logged.contains("AUTH"),
        "{logged}"
    );
}

#[test]
fn switching_off_removes_the_subscription_from_the_gateway_and_the_browser() {
    let mut fixture = Fixture::new();
    fixture.notifications_on();
    fixture.click("Notifications on this device");
    assert_eq!(fixture.sent(), [ApiRequest::DeletePushSubscription]);
    assert_eq!(fixture.push_controls(), [DeviceControl::Unsubscribe]);
    assert_eq!(fixture.fact("push"), "unsubscribing".into());
    assert_eq!(fixture.fact("push_saved"), false.into());
    fixture.reply(ApiReply::PushUnsubscribed(Ok(())));
    fixture.push(PushEvent::Unsubscribed);
    assert_eq!(fixture.fact("push"), "off".into());
    assert!(fixture.sent().is_empty());
    // And on again, with a new subscription.
    fixture.click("Notifications on this device");
    assert_eq!(
        fixture.push_controls(),
        [DeviceControl::Subscribe("SERVER-KEY".into())]
    );
    fixture.push(PushEvent::Subscribed(subscription()));
    assert_eq!(fixture.sent(), [request()]);
}

#[test]
fn a_launch_with_a_live_session_sends_the_devices_subscription_again() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.sent(), [ApiRequest::CheckSession]);
    fixture.push(state(NotificationPermission::Granted, true));
    // Nothing while the session is checked.
    assert!(fixture.push_controls().is_empty());
    assert!(fixture.sent().is_empty());
    fixture.reply(ApiReply::Session(Ok(session(user()))));
    assert_eq!(
        fixture.sent(),
        [
            ApiRequest::ListBots,
            ApiRequest::LoadUsage,
            ApiRequest::CodexAccount,
            ApiRequest::PushKey,
            request()
        ]
    );
    assert_eq!(fixture.fact("push"), "on".into());
    fixture.reply(ApiReply::PushSubscribed(Ok(PushSubscriptionState {
        subscribed: true,
    })));
    assert_eq!(fixture.fact("push_saved"), true.into());
    assert!(
        fixture.enabled(BokkieAction::Notifications),
        "it can go off"
    );
    assert!(fixture.push_controls().is_empty());
}

#[test]
fn signing_out_removes_the_devices_subscription() {
    let mut fixture = Fixture::new();
    fixture.notifications_on();
    fixture.click("Sign out");
    let _ = fixture.sent();
    fixture.reply(ApiReply::SignedOut(Ok(())));
    // The gateway removed it with the session; the browser removes it too.
    let device = fixture.push_controls();
    assert!(device.contains(&DeviceControl::Unsubscribe), "{device:?}");
    assert_eq!(fixture.fact("push"), "unsubscribing".into());
    fixture.push(PushEvent::Unsubscribed);
    assert_eq!(fixture.fact("push"), "off".into());
    assert_eq!(
        availability(&fixture),
        Availability::Disabled {
            reason: "Not signed in".into()
        }
    );
}

#[test]
fn an_ended_session_found_at_launch_removes_the_devices_subscription() {
    let mut fixture = Fixture::new();
    fixture.push(state(NotificationPermission::Granted, true));
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    assert_eq!(fixture.push_controls(), [DeviceControl::Unsubscribe]);
}

#[test]
fn a_new_sign_in_never_takes_over_an_earlier_subscription() {
    let mut fixture = Fixture::new();
    fixture.push(state(NotificationPermission::Granted, true));
    // An unreachable server says nothing about the session: keep it.
    fixture.reply(ApiReply::Session(Err(CommandError::unreachable())));
    assert!(fixture.push_controls().is_empty());
    // Someone signs in: the device's subscription is not theirs.
    let _ = fixture.sent();
    fixture.sign_in();
    assert_eq!(fixture.push_controls(), [DeviceControl::Unsubscribe]);
    assert_eq!(
        availability(&fixture),
        Availability::Disabled {
            reason: "Switching notifications off".into()
        }
    );
    fixture.push(PushEvent::Unsubscribed);
    assert_eq!(fixture.sent(), [ApiRequest::PushKey]);
    fixture.reply(key(true));
    assert_eq!(fixture.fact("push"), "off".into());
    assert!(fixture.sent().is_empty(), "nothing was sent to the gateway");
}

#[test]
fn a_failed_removal_is_neither_tried_again_nor_sent() {
    let mut fixture = Fixture::new();
    fixture.push(state(NotificationPermission::Granted, true));
    fixture.reply(ApiReply::Session(Err(unauthenticated())));
    assert_eq!(fixture.push_controls(), [DeviceControl::Unsubscribe]);
    fixture.push(PushEvent::Failed {
        operation: PushOperation::Unsubscribe,
        message: "The service worker is not active".into(),
    });
    fixture.send(BokkieIntent::EditUsername("ada".into()));
    fixture.send(BokkieIntent::EditPassword("a long password".into()));
    fixture.click("Sign in");
    let _ = fixture.sent();
    fixture.reply(ApiReply::SignedIn(Ok(session(user()))));
    // Each new sign-in tries once.
    assert_eq!(fixture.push_controls(), [DeviceControl::Unsubscribe]);
    fixture.push(PushEvent::Failed {
        operation: PushOperation::Unsubscribe,
        message: "The service worker is not active".into(),
    });
    assert!(fixture.sent().contains(&ApiRequest::PushKey));
    fixture.reply(key(true));
    assert!(fixture.push_controls().is_empty());
    assert!(
        fixture.sent().is_empty(),
        "an earlier sign-in's subscription is never sent"
    );
    assert_eq!(fixture.fact("push"), "off".into());
    assert_eq!(
        fixture.fact("push_error"),
        "The service worker is not active".into()
    );
}

#[test]
fn a_device_without_push_or_permission_says_why() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert_eq!(
        availability(&fixture),
        Availability::Disabled {
            reason: "Checking this device for notifications".into()
        }
    );
    fixture.push(PushEvent::State(PushState {
        support: PushSupport::Unsupported("Add Bokkie to the Home Screen first".into()),
        permission: None,
        subscription: None,
    }));
    assert_eq!(fixture.fact("push"), "unsupported".into());
    assert_eq!(
        availability(&fixture),
        Availability::Disabled {
            reason: "Add Bokkie to the Home Screen first".into()
        }
    );
    assert!(
        fixture.sent().is_empty(),
        "no key for a device without push"
    );
    fixture.push(state(NotificationPermission::Denied, false));
    let _ = fixture.sent();
    fixture.reply(key(true));
    assert_eq!(fixture.fact("push"), "denied".into());
    assert_eq!(
        availability(&fixture),
        Availability::Disabled {
            reason: "Notifications are blocked in this browser's settings".into()
        }
    );
}

#[test]
fn a_refused_subscription_and_a_gateway_that_does_not_send_are_said() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.push(state(NotificationPermission::Default, false));
    let _ = fixture.sent();
    fixture.reply(key(false));
    assert_eq!(fixture.fact("push_sending"), false.into());
    fixture.reveal("Notifications on this device");
    assert!(fixture.shows("This Bokkie server is not set up to send notifications yet"));
    fixture.click("Notifications on this device");
    fixture.push(PushEvent::Failed {
        operation: PushOperation::Subscribe,
        message: "Permission was not granted".into(),
    });
    assert_eq!(fixture.fact("push"), "off".into());
    assert_eq!(
        fixture.fact("push_error"),
        "Permission was not granted".into()
    );
    assert!(fixture.enabled(BokkieAction::Notifications), "try again");
}

#[test]
fn the_socket_hears_which_bot_the_page_shows() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert!(
        fixture.sent.take_viewing().is_empty(),
        "the list shows none"
    );
    fixture.open_tutor(hello(), 10);
    assert_eq!(fixture.sent.take_viewing(), [Some(TUTOR)]);
    assert_eq!(fixture.fact("viewing"), 1_i64.into());
    fixture.click("Settings");
    assert_eq!(fixture.sent.take_viewing(), [None]);
    fixture.click("Back");
    assert_eq!(fixture.sent.take_viewing(), [Some(TUTOR)]);
    fixture.click("Back");
    assert_eq!(fixture.sent.take_viewing(), [None]);
}

/// The Tutor unread after `read_up_to`, listed and live after event 10.
fn unread_tutor(fixture: &mut Fixture, read_up_to: i64) {
    fixture.signed_in_and_live(10);
    let mut list = tutor_list(BotStatus::Idle, 10);
    list.bots[0].unread = true;
    list.bots[0].read_up_to = Some(MessageId(read_up_to));
    list.unread_bots = 1;
    fixture.click("Refresh Bots");
    assert!(fixture.sent().contains(&ApiRequest::ListBots));
    fixture.reply(ApiReply::Bots(Ok(list)));
}

#[test]
fn an_unread_bot_is_marked_in_the_list_and_counted_on_the_badge() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    assert_eq!(fixture.sent.take_device(), [DeviceControl::Badge(0)]);
    fixture.event(
        11,
        Event::MessageCreated {
            message: message(5, Author::Bot, "Done", MessageState::Complete),
        },
    );
    assert_eq!(fixture.fact("unread"), "1".into());
    assert_eq!(fixture.fact("unread_bots"), 1_u64.into());
    // The row leads with the unread words, beside its unread dot.
    assert!(fixture.row_says("Tutor").starts_with("Unread; Done; "));
    assert_eq!(fixture.sent.take_device(), [DeviceControl::Badge(1)]);
    assert_eq!(fixture.fact("badge"), 1_u64.into());
    // The list does not read the Bot.
    assert!(fixture.sent().is_empty());
    // Read on another device.
    fixture.event(12, read(5, false));
    assert_eq!(fixture.fact("unread"), "".into());
    assert!(fixture.row_says("Tutor").starts_with("Done; "));
    assert_eq!(fixture.sent.take_device(), [DeviceControl::Badge(0)]);
}

#[test]
fn the_badge_counts_hidden_bots_and_clears_on_sign_out() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    let mut list = tutor_list(BotStatus::Idle, 10);
    let mut hidden = bot(2, "Gardener", "Plans the garden", BotStatus::Idle, None);
    hidden.hidden = true;
    hidden.unread = true;
    list.bots.push(hidden);
    list.unread_bots = 1;
    fixture.click("Refresh Bots");
    let _ = fixture.sent();
    fixture.reply(ApiReply::Bots(Ok(list)));
    assert!(!fixture.shows("Gardener"), "hidden from the list");
    assert_eq!(fixture.fact("unread"), "2".into());
    assert_eq!(
        fixture.sent.take_device().last(),
        Some(&DeviceControl::Badge(1))
    );
    fixture.click("Sign out");
    fixture.reply(ApiReply::SignedOut(Ok(())));
    assert_eq!(fixture.sent.take_device(), [DeviceControl::Badge(0)]);
}

#[test]
fn the_read_marker_moves_while_the_conversation_is_on_screen() {
    let mut fixture = Fixture::new();
    unread_tutor(&mut fixture, 1);
    fixture.open_tutor(hello(), 10);
    assert_eq!(fixture.fact("bot_unread"), true.into());
    assert_eq!(fixture.sent(), [mark_read(TUTOR, 2)]);
    fixture.reply(ApiReply::MarkedRead(
        TUTOR,
        MessageId(2),
        Ok(ReadState {
            bot_id: TUTOR,
            read_up_to: Some(MessageId(2)),
            unread: false,
        }),
    ));
    // The store follows the event, which keeps order with new messages.
    fixture.event(11, read(2, false));
    assert_eq!(fixture.fact("bot_unread"), false.into());
    assert_eq!(fixture.fact("read_up_to"), 2_i64.into());
    // A new reply on screen is read at once.
    fixture.event(
        12,
        Event::MessageCreated {
            message: message(3, Author::Bot, "More", MessageState::Complete),
        },
    );
    assert_eq!(fixture.sent(), [mark_read(TUTOR, 3)]);
}

#[test]
fn nothing_is_read_while_the_page_is_hidden_or_the_settings_are_open() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.open_tutor(hello(), 10);
    fixture.send(BokkieIntent::Background);
    let created = |id| Event::MessageCreated {
        message: message(id, Author::Bot, "More", MessageState::Complete),
    };
    fixture.event(11, created(3));
    assert_eq!(fixture.fact("bot_unread"), true.into());
    assert!(fixture.sent().is_empty());
    fixture.send(BokkieIntent::Foreground);
    assert_eq!(fixture.sent(), [mark_read(TUTOR, 3)]);
    fixture.reply(ApiReply::MarkedRead(
        TUTOR,
        MessageId(3),
        Ok(ReadState {
            bot_id: TUTOR,
            read_up_to: Some(MessageId(3)),
            unread: false,
        }),
    ));
    fixture.event(12, read(3, false));
    fixture.click("Settings");
    let _ = fixture.sent();
    fixture.event(13, created(4));
    assert!(
        !fixture.sent().contains(&mark_read(TUTOR, 4)),
        "the settings do not show the conversation"
    );
    fixture.click("Back");
    assert_eq!(fixture.sent(), [mark_read(TUTOR, 4)]);
}

#[test]
fn a_refused_read_marker_is_not_sent_again_in_a_loop() {
    let mut fixture = Fixture::new();
    unread_tutor(&mut fixture, 1);
    fixture.open_tutor(hello(), 10);
    assert_eq!(fixture.sent(), [mark_read(TUTOR, 2)]);
    fixture.reply(ApiReply::MarkedRead(
        TUTOR,
        MessageId(2),
        Err(CommandError::unreachable()),
    ));
    fixture.event(11, read(1, true));
    assert!(fixture.sent().is_empty());
    // Opening the conversation again tries again.
    fixture.click("Back");
    fixture.open("Tutor");
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(11),
        }),
    ));
    assert_eq!(
        fixture.sent(),
        [ApiRequest::LoadMessages(TUTOR), mark_read(TUTOR, 2)]
    );
}

#[test]
fn a_tapped_notification_opens_its_bot() {
    let mut fixture = Fixture::new();
    fixture.signed_in_and_live(10);
    fixture.push(route("?bot=1"));
    assert_eq!(fixture.fact("screen"), "conversation".into());
    assert_eq!(fixture.fact("bot"), 1_i64.into());
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
    // From the Bot's settings, the conversation shows again.
    fixture.reply(ApiReply::Messages(
        TUTOR,
        Ok(MessagePage {
            messages: hello(),
            older: None,
            seq: Seq(10),
        }),
    ));
    fixture.click("Settings");
    let _ = fixture.sent();
    fixture.push(route("?bot=1"));
    assert_eq!(fixture.fact("screen"), "conversation".into());
    // A Bot this account does not have, or a route that names none, does
    // nothing.
    fixture.click("Back");
    fixture.push(route("?bot=99"));
    fixture.push(route(""));
    assert_eq!(fixture.fact("screen"), "signed_in".into());
}

#[test]
fn a_notification_tapped_before_signing_in_opens_its_bot_once_listed() {
    let mut fixture = Fixture::new();
    fixture.push(route("?bot=1"));
    fixture.reply(ApiReply::Session(Ok(session(user()))));
    assert_eq!(fixture.fact("screen"), "signed_in".into());
    let _ = fixture.sent();
    fixture.reply(ApiReply::Bots(Ok(tutor_list(BotStatus::Idle, 4))));
    assert_eq!(fixture.fact("screen"), "conversation".into());
    assert_eq!(fixture.sent(), [ApiRequest::LoadMessages(TUTOR)]);
}

#[test]
fn routes_name_their_bot() {
    let route = |path: &str| NotificationRoute {
        url: String::new(),
        path: path.into(),
    };
    let bot = |path| crate::notifications::routed_bot(&route(path));
    assert_eq!(bot("?bot=12"), Some(BotId(12)));
    assert_eq!(bot("?x=1&bot=3"), Some(BotId(3)));
    assert_eq!(bot("?bot=0"), None);
    assert_eq!(bot("?bot=-4"), None);
    assert_eq!(bot("?bot=tutor"), None);
    assert_eq!(bot(""), None);
}
