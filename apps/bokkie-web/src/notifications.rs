//! The device's notifications, and what the user has read.
//!
//! - **Notifications.** Each device has its own switch. Switching it on
//!   subscribes the browser to Web Push with the gateway's public key, from
//!   the tap itself, and sends the subscription to the gateway, which keeps
//!   one per session. A launch with a live session sends the device's
//!   subscription again, because Safari never reports a changed one.
//!   Switching it off removes the subscription from the gateway and the
//!   browser; signing out removes it from the browser too, since the
//!   gateway removed it with the session. A new sign-in never takes over a
//!   subscription an earlier session left on the device: it removes it, and
//!   the user switches notifications on again. A switch-off the browser
//!   failed leaves the subscription unsent until the next launch or the
//!   user switches notifications on again; a subscription the gateway
//!   refused is sent once more each time the socket reloads.
//! - **Reading.** While a conversation is on screen with the page in the
//!   foreground, the gateway hears that the device shows that Bot (the
//!   socket's `viewing` frames), so it sends no push for it, and the read
//!   marker moves to the conversation's newest message while the Bot is
//!   unread. The Bot list's unread state and the app badge follow the
//!   gateway's `read_updated` and `message_created` events, so every device
//!   agrees. The badge counts unread Bots, hidden ones too.
//! - **Routes.** A tapped notification opens its Bot, once signed in.
//!
//! An endpoint and the subscription's keys never reach a fact or a log
//! line: anyone who holds the endpoint can address the device.

use std::borrow::Cow;

use bokkie_protocol::api::{MarkReadRequest, PushKey, PushSubscriptionRequest};
use bokkie_protocol::{BotId, MessageId};
use stobie_ui_egui::Observation;
use stobie_ui_egui::web_push::{
    NotificationPermission, NotificationRoute, PushEvent, PushOperation, PushState, PushSupport,
};

use crate::api::{ApiReply, ApiRequest};
use crate::app::{App, Open, Session};

/// The device's Web Push and badge, which outlast a sign-in.
#[derive(Default)]
pub(crate) struct Device {
    /// Support, permission and the subscription, as Web Push last said;
    /// `None` until it has.
    state: Option<PushState>,
    busy: Option<Busy>,
    /// Why the last subscribe or unsubscribe failed.
    error: Option<String>,
    /// Whether to remove the device's subscription, which no live session
    /// holds after signing out, and which a new sign-in does not take over.
    forget: Forget,
    /// The Bot a tapped notification asked for, until it can open.
    route: Option<BotId>,
    /// The count last shown on the app's icon.
    badge: Option<u64>,
    /// The Bot the event socket was last told the page shows.
    viewing: Option<BotId>,
    /// The page is hidden, so nothing on it is being read.
    hidden: bool,
    /// The user switched notifications off: whatever subscription the
    /// browser still holds is not sent until a deliberate switch-on or the
    /// next launch.
    held: bool,
}

// The subscription's endpoint must never reach a log line.
impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("push", &self.push_name())
            .field("badge", &self.badge)
            .field("viewing", &self.viewing)
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Forget {
    /// The subscription, if any, is this sign-in's.
    #[default]
    No,
    /// Remove it once Web Push has said what the device holds.
    Pending,
    /// Removing it failed: neither try again nor send it to the gateway.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Subscribing,
    Unsubscribing,
}

impl Device {
    fn subscribed(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.subscription.is_some())
    }

    /// The `push` fact: empty until Web Push has reported, then
    /// `unsupported`, `denied`, `subscribing`, `unsubscribing`, `on` or
    /// `off`.
    fn push_name(&self) -> &'static str {
        let Some(state) = &self.state else {
            return "";
        };
        match (self.busy, &state.support) {
            (_, PushSupport::Unsupported(_)) => "unsupported",
            (Some(Busy::Subscribing), _) => "subscribing",
            (Some(Busy::Unsubscribing), _) => "unsubscribing",
            _ if state.subscription.is_some() && self.forget == Forget::No && !self.held => "on",
            _ if state.permission == Some(NotificationPermission::Denied) => "denied",
            _ => "off",
        }
    }
}

/// What one sign-in holds of the device's notifications and reading.
#[derive(Default)]
pub(crate) struct Signed {
    key: Key,
    /// The endpoint this sign-in last sent the gateway, so that each
    /// subscription is sent once.
    sent: Option<String>,
    /// The subscription is being sent.
    saving: bool,
    /// The gateway holds this session's subscription.
    saved: bool,
    /// Why the gateway refused the subscription, or the key.
    error: Option<String>,
    /// The gateway refused the subscription last sent, other than for the
    /// session: the next reload sends it once more.
    refused: bool,
    /// The read marker being sent.
    marking: Option<(BotId, MessageId)>,
    /// The read marker last sent, answered or not, which is not sent
    /// again.
    marked: Option<(BotId, MessageId)>,
}

impl Signed {
    /// A conversation opened: its read marker may be sent again.
    pub(crate) fn opened(&mut self) {
        self.marked = None;
    }
}

impl std::fmt::Debug for Signed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signed")
            .field("saving", &self.saving)
            .field("saved", &self.saved)
            .field("marking", &self.marking)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
enum Key {
    #[default]
    NotLoaded,
    Loading,
    Loaded(PushKey),
    Failed,
}

/// A [`PushEvent`] for a log line: never the endpoint or the keys.
pub(crate) fn describe(event: &PushEvent) -> String {
    match event {
        PushEvent::State(state) => format!(
            "State {{ support: {:?}, permission: {:?}, subscribed: {} }}",
            state.support,
            state.permission,
            state.subscription.is_some()
        ),
        PushEvent::Subscribed(_) => "Subscribed".into(),
        PushEvent::Unsubscribed => "Unsubscribed".into(),
        PushEvent::Failed { operation, message } => format!("Failed({operation:?}, {message:?})"),
        PushEvent::Route(route) => format!("Route({:?})", route.path),
    }
}

/// The Bot a notification's route names: the gateway sends `?bot=<id>`.
pub(crate) fn routed_bot(route: &NotificationRoute) -> Option<BotId> {
    let query = route.path.split_once('?').map_or("", |(_, query)| query);
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("bot="))
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .map(BotId)
}

type Reason = Option<Cow<'static, str>>;

impl App {
    fn signed(&self) -> Option<&Signed> {
        match &self.session {
            Session::SignedIn(signed_in) => Some(&signed_in.push),
            _ => None,
        }
    }

    fn signed_mut(&mut self) -> Option<&mut Signed> {
        match &mut self.session {
            Session::SignedIn(signed_in) => Some(&mut signed_in.push),
            _ => None,
        }
    }

    /// A sign-in is starting, or the session has gone: whatever
    /// subscription the device holds is no longer this sign-in's.
    pub(crate) fn forget_subscription(&mut self) {
        self.device.forget = Forget::Pending;
    }

    /// Whether the switch shows notifications on.
    pub(crate) fn notifications_on(&self) -> bool {
        self.device.subscribed() && self.device.forget == Forget::No && !self.device.held
    }

    /// The notifications' state, when it is worth saying.
    pub(crate) fn notifications_status(&self) -> Option<(Cow<'static, str>, bool)> {
        let signed = self.signed()?;
        let state = self.device.state.as_ref()?;
        if let PushSupport::Unsupported(reason) = &state.support {
            return Some((reason.clone().into(), false));
        }
        if let Some(error) = signed.error.as_ref().or(self.device.error.as_ref()) {
            return Some((error.clone().into(), true));
        }
        Some(match self.device.busy {
            Some(Busy::Subscribing) => ("Switching notifications on…".into(), false),
            Some(Busy::Unsubscribing) => ("Switching notifications off…".into(), false),
            None if state.permission == Some(NotificationPermission::Denied) => (
                "Notifications are blocked in this browser's settings".into(),
                false,
            ),
            None if matches!(&signed.key, Key::Loaded(key) if !key.sending) => (
                "This Bokkie server is not set up to send notifications yet".into(),
                false,
            ),
            None if signed.saving => ("Saving…".into(), false),
            None => return None,
        })
    }

    /// Whether the notifications switch can be used now.
    pub(crate) fn notifications_availability(&self) -> Reason {
        let because = |reason: &'static str| Some(Cow::Borrowed(reason));
        let Some(signed) = self.signed() else {
            return because("Not signed in");
        };
        let Some(state) = &self.device.state else {
            return because("Checking this device for notifications");
        };
        if let PushSupport::Unsupported(reason) = &state.support {
            return Some(Cow::Owned(reason.clone()));
        }
        match (self.device.busy, &signed.key) {
            (Some(Busy::Subscribing), _) => because("Switching notifications on"),
            (Some(Busy::Unsubscribing), _) => because("Switching notifications off"),
            _ if self.device.forget == Forget::Pending => {
                because("Removing an earlier sign-in's notifications")
            }
            _ if signed.saving => because("Saving the subscription"),
            _ if self.notifications_on() => None,
            _ if state.permission == Some(NotificationPermission::Denied) => {
                because("Notifications are blocked in this browser's settings")
            }
            (_, Key::Loaded(_)) => None,
            (_, Key::Failed) => because("Bokkie's push key could not be loaded"),
            _ => because("Loading Bokkie's push key"),
        }
    }

    /// The switch: subscribe from the tap itself, or unsubscribe.
    pub(crate) fn toggle_notifications(&mut self) -> bool {
        if self.notifications_on() {
            self.device.busy = Some(Busy::Unsubscribing);
            self.device.error = None;
            // Should the browser fail to remove it, the subscription it
            // still holds is not sent again behind the user's back.
            self.device.held = true;
            if let Some(signed) = self.signed_mut() {
                signed.sent = None;
                signed.saved = false;
                signed.error = None;
                signed.refused = false;
            }
            self.send(ApiRequest::DeletePushSubscription);
            if let Some(api) = &self.api {
                api.unsubscribe_push();
            }
            return true;
        }
        let Some(Signed {
            key: Key::Loaded(key),
            ..
        }) = self.signed()
        else {
            return false;
        };
        let key = key.public_key.clone();
        self.device.busy = Some(Busy::Subscribing);
        self.device.error = None;
        self.device.held = false;
        if let Some(signed) = self.signed_mut() {
            signed.error = None;
            signed.refused = false;
        }
        if let Some(api) = &self.api {
            api.subscribe_push(&key);
        }
        true
    }

    /// A result from Web Push.
    pub(crate) fn push_event(&mut self, event: PushEvent) -> bool {
        let signed_in = matches!(self.session, Session::SignedIn(_));
        let device = &mut self.device;
        match event {
            PushEvent::State(state) => {
                if state.subscription.is_none() {
                    device.forget = Forget::No;
                }
                device.state = Some(state);
            }
            PushEvent::Subscribed(subscription) => {
                if device.busy == Some(Busy::Subscribing) {
                    device.busy = None;
                }
                // The user switched notifications on for this sign-in.
                if signed_in {
                    device.forget = Forget::No;
                }
                device.held = false;
                let state = device.state.get_or_insert(PushState {
                    support: PushSupport::Supported,
                    permission: None,
                    subscription: None,
                });
                state.permission = Some(NotificationPermission::Granted);
                state.subscription = Some(subscription);
            }
            PushEvent::Unsubscribed => {
                if device.busy == Some(Busy::Unsubscribing) {
                    device.busy = None;
                }
                device.forget = Forget::No;
                device.held = false;
                if let Some(state) = &mut device.state {
                    state.subscription = None;
                }
            }
            PushEvent::Failed { operation, message } => {
                device.busy = None;
                // A refused permission is the browser's to say.
                device.error = Some(message);
                if operation == PushOperation::Unsubscribe && device.forget == Forget::Pending {
                    // Not again in a loop.
                    device.forget = Forget::Failed;
                }
            }
            PushEvent::Route(route) => match routed_bot(&route) {
                Some(bot) => device.route = Some(bot),
                None => return false,
            },
        }
        true
    }

    /// The page was hidden or shown again.
    pub(crate) fn set_hidden(&mut self, hidden: bool) -> bool {
        let changed = self.device.hidden != hidden;
        self.device.hidden = hidden;
        changed
    }

    /// The reply to a push or read command, or the reply back for others.
    pub(crate) fn push_reply(&mut self, reply: ApiReply) -> Result<bool, Box<ApiReply>> {
        let Session::SignedIn(signed_in) = &mut self.session else {
            return match reply {
                ApiReply::PushKey(_)
                | ApiReply::PushSubscribed(_)
                | ApiReply::PushUnsubscribed(_)
                | ApiReply::MarkedRead(..) => Ok(false),
                reply => Err(Box::new(reply)),
            };
        };
        let signed = &mut signed_in.push;
        let ended = match reply {
            ApiReply::PushKey(result) => {
                if !matches!(signed.key, Key::Loading) {
                    return Ok(false);
                }
                match result {
                    Ok(key) => {
                        signed.key = Key::Loaded(key);
                        false
                    }
                    Err(error) if error.is_unauthenticated() => true,
                    Err(error) => {
                        signed.key = Key::Failed;
                        signed.error = Some(error.message);
                        false
                    }
                }
            }
            ApiReply::PushSubscribed(result) => {
                if !signed.saving {
                    return Ok(false);
                }
                signed.saving = false;
                match result {
                    Ok(state) => {
                        signed.saved = state.subscribed;
                        false
                    }
                    Err(error) if error.is_unauthenticated() => true,
                    Err(error) => {
                        signed.error = Some(error.message);
                        signed.refused = true;
                        false
                    }
                }
            }
            ApiReply::PushUnsubscribed(result) => match result {
                Ok(()) => return Ok(false),
                Err(error) if error.is_unauthenticated() => true,
                Err(error) => {
                    signed.error = Some(error.message);
                    false
                }
            },
            ApiReply::MarkedRead(bot, message, result) => {
                if signed.marking != Some((bot, message)) {
                    return Ok(false);
                }
                signed.marking = None;
                // The marker's move arrives as `read_updated`, in order
                // with the Bot's new messages, so the store follows that
                // rather than this answer, which a newer message may have
                // overtaken.
                match result {
                    Ok(_) => return Ok(false),
                    Err(error) => error.is_unauthenticated(),
                }
            }
            reply => return Err(Box::new(reply)),
        };
        if ended {
            self.session_ended();
        }
        Ok(true)
    }

    /// The socket is live again after missing events: try what failed,
    /// once. A refused subscription is sent again on the next pass, and
    /// refused again only waits for the next reload.
    pub(crate) fn retry_push(&mut self) {
        let Some(signed) = self.signed_mut() else {
            return;
        };
        if matches!(signed.key, Key::Failed) {
            signed.key = Key::NotLoaded;
            signed.error = None;
        }
        if signed.refused && !signed.saving {
            signed.refused = false;
            signed.sent = None;
            signed.error = None;
        }
    }

    /// Bring the device and the gateway in line with the state, once a
    /// pass: the push key and subscription, the Bot the page shows, the
    /// read marker, the badge, and a tapped notification's Bot.
    pub(crate) fn sync(&mut self) {
        self.sync_subscription();
        self.sync_viewing();
        self.sync_read();
        self.sync_badge();
        self.sync_route();
    }

    fn sync_subscription(&mut self) {
        let Some(state) = &self.device.state else {
            return;
        };
        if !state.support.is_supported() || self.device.busy.is_some() {
            return;
        }
        let endpoint = state.subscription.as_ref().map(|s| s.endpoint.clone());
        match self.device.forget {
            Forget::No | Forget::Failed => {}
            // Nothing to remove.
            Forget::Pending if endpoint.is_none() => self.device.forget = Forget::No,
            Forget::Pending => {
                self.device.busy = Some(Busy::Unsubscribing);
                if let Some(api) = &self.api {
                    api.unsubscribe_push();
                }
                return;
            }
        }
        if matches!(self.session, Session::Checking) {
            return;
        }
        let json = state.subscription.as_ref().map(|s| s.json.clone());
        let adopt = self.device.forget == Forget::No && !self.device.held;
        let Some(signed) = self.signed_mut() else {
            return;
        };
        if matches!(signed.key, Key::NotLoaded) {
            signed.key = Key::Loading;
            self.send(ApiRequest::PushKey);
            return;
        }
        let (Some(endpoint), Some(json), true) = (endpoint, json, adopt) else {
            return;
        };
        if signed.saving || signed.sent.as_deref() == Some(endpoint.as_str()) {
            return;
        }
        signed.sent = Some(endpoint);
        match serde_json::from_str::<PushSubscriptionRequest>(&json) {
            Ok(request) => {
                signed.saving = true;
                self.send(ApiRequest::PutPushSubscription(request));
            }
            Err(_) => {
                signed.error = Some("The browser's subscription could not be read".into());
            }
        }
    }

    /// The Bot whose conversation the page shows, if any. The socket says
    /// none while the page is hidden.
    fn sync_viewing(&mut self) {
        let shown = self
            .conversation_visible()
            .then(|| self.open_bot())
            .flatten();
        if shown != self.device.viewing {
            self.device.viewing = shown;
            if let Some(api) = &self.api {
                api.set_viewing(shown);
            }
        }
    }

    /// Move the read marker to the newest message of the conversation on
    /// screen while its Bot is unread.
    fn sync_read(&mut self) {
        if self.device.hidden || !self.conversation_visible() {
            return;
        }
        let Some(bot) = self.open_bot() else {
            return;
        };
        if !self.store.bot(bot).is_some_and(|bot| bot.unread) {
            return;
        }
        let Some(newest) = self
            .store
            .conversation(bot)
            .and_then(|conversation| conversation.messages().next_back())
            .map(|message| message.id)
        else {
            return;
        };
        let Some(signed) = self.signed_mut() else {
            return;
        };
        let target = Some((bot, newest));
        if signed.marking.is_some() || signed.marked == target {
            return;
        }
        signed.marking = target;
        signed.marked = target;
        self.send(ApiRequest::MarkRead(
            bot,
            MarkReadRequest { message_id: newest },
        ));
    }

    /// How many Bots are unread, hidden ones too, or `None` before the list
    /// has loaded.
    pub(crate) fn unread_bots(&self) -> Option<u64> {
        match &self.session {
            Session::SignedIn(_) if self.store.bots_loaded() => {
                Some(self.store.bots().filter(|bot| bot.unread).count() as u64)
            }
            Session::SignedOut(_) => Some(0),
            _ => None,
        }
    }

    fn sync_badge(&mut self) {
        let Some(count) = self.unread_bots() else {
            return;
        };
        if self.device.badge != Some(count) {
            self.device.badge = Some(count);
            if let Some(api) = &self.api {
                api.set_app_badge(count);
            }
        }
    }

    /// Open the Bot of a tapped notification, once the list knows it.
    fn sync_route(&mut self) {
        if self.device.route.is_none() || !self.store.bots_loaded() {
            return;
        }
        let Some(bot) = self.device.route.take() else {
            return;
        };
        // Another account's Bot, or one deleted since.
        if self.store.bot(bot).is_none() {
            return;
        }
        let Session::SignedIn(signed_in) = &mut self.session else {
            return;
        };
        signed_in.household = None;
        signed_in.admin = None;
        if signed_in.open.as_ref().is_some_and(|open| open.bot == bot) {
            self.close_settings();
        } else {
            let _ = self.open_conversation(bot);
        }
    }

    /// The facts of notifications and reading. Never an endpoint or a key.
    pub(crate) fn observe_device(&self, observation: &mut Observation<'_>) {
        let signed = self.signed();
        observation.fact("push", self.device.push_name());
        observation.fact("push_saved", signed.is_some_and(|s| s.saved));
        observation.fact(
            "push_sending",
            matches!(signed.map(|s| &s.key), Some(Key::Loaded(key)) if key.sending),
        );
        observation.fact(
            "push_error",
            crate::app::bounded(
                signed
                    .and_then(|s| s.error.as_deref())
                    .or(self.device.error.as_deref())
                    .unwrap_or(""),
            ),
        );
        observation.fact("badge", self.device.badge.unwrap_or(0));
        observation.fact("viewing", self.device.viewing.map_or(0, |bot| bot.0));
        let mut unread: Vec<i64> = match &self.session {
            Session::SignedIn(_) => self
                .store
                .bots()
                .filter(|bot| bot.unread)
                .map(|bot| bot.id.0)
                .collect(),
            _ => Vec::new(),
        };
        unread.sort_unstable();
        observation.fact("unread_bots", unread.len());
        let unread: Vec<String> = unread.iter().map(i64::to_string).collect();
        observation.fact("unread", unread.join(","));
        let open = self.open().and_then(|open: &Open| self.store.bot(open.bot));
        observation.fact("bot_unread", open.is_some_and(|bot| bot.unread));
        observation.fact(
            "read_up_to",
            open.and_then(|bot| bot.read_up_to).map_or(0, |id| id.0),
        );
    }
}
