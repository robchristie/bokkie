//! The HTTP commands the client sends, independent of how they travel.
//!
//! Each [`ApiRequest`] knows its method, path and body, and how to decode
//! its response into an [`ApiReply`]. The browser transport
//! (`crate::browser`) sends them with `fetch` and runs the event socket;
//! tests record them and reply by hand.

use bokkie_protocol::api::{
    Bot, BotList, CodexAccount, CreateBotRequest, DeviceCodeLogin, LoginRequest, MessagePage,
    SendMessageRequest, SendMessageResponse, SessionResponse, StopResponse, Usage,
};
use bokkie_protocol::{API_PREFIX, BotId, MessageId, Timestamp};

use crate::wire::{self, CommandError};

/// A command for the gateway.
#[derive(Clone, PartialEq, Eq)]
pub enum ApiRequest {
    /// `GET /api/v1/session`: whether the browser holds a valid session.
    CheckSession,
    /// `POST /api/v1/session`: sign in.
    SignIn(LoginRequest),
    /// `DELETE /api/v1/session`: sign this browser out.
    SignOut,
    /// `GET /api/v1/bots`: the signed-in user's Bots.
    ListBots,
    /// `GET /api/v1/bots/{id}/messages`: the newest page of a conversation.
    LoadMessages(BotId),
    /// `GET /api/v1/bots/{id}/messages?before={message_id}`: the page of a
    /// conversation before that message.
    LoadOlder(BotId, MessageId),
    /// `POST /api/v1/bots/{id}/messages`: send a message, which starts a
    /// turn or steers the running one.
    SendMessage(BotId, SendMessageRequest),
    /// `POST /api/v1/bots/{id}/stop`: interrupt the running reply.
    StopBot(BotId),
    /// `GET /api/v1/usage`: the usage meter.
    LoadUsage,
    /// `POST /api/v1/bots`: create a Bot.
    CreateBot(CreateBotRequest),
    /// `GET /api/v1/codex/account`: whether the user's own app server is
    /// signed in to ChatGPT.
    CodexAccount,
    /// `POST /api/v1/codex/login`: start a device-code sign-in.
    CodexLogin,
}

// The password must never reach a log line.
impl std::fmt::Debug for ApiRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CheckSession => f.write_str("CheckSession"),
            Self::SignIn(login) => f.debug_tuple("SignIn").field(login).finish(),
            Self::SignOut => f.write_str("SignOut"),
            Self::ListBots => f.write_str("ListBots"),
            Self::LoadMessages(bot) => f.debug_tuple("LoadMessages").field(bot).finish(),
            Self::LoadOlder(bot, before) => {
                f.debug_tuple("LoadOlder").field(bot).field(before).finish()
            }
            Self::SendMessage(bot, message) => f
                .debug_tuple("SendMessage")
                .field(bot)
                .field(message)
                .finish(),
            Self::StopBot(bot) => f.debug_tuple("StopBot").field(bot).finish(),
            Self::LoadUsage => f.write_str("LoadUsage"),
            Self::CreateBot(bot) => f.debug_tuple("CreateBot").field(bot).finish(),
            Self::CodexAccount => f.write_str("CodexAccount"),
            Self::CodexLogin => f.write_str("CodexLogin"),
        }
    }
}

/// An HTTP method the client uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

/// The answer to an [`ApiRequest`], decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiReply {
    Session(Result<SessionResponse, CommandError>),
    SignedIn(Result<SessionResponse, CommandError>),
    SignedOut(Result<(), CommandError>),
    Bots(Result<BotList, CommandError>),
    Messages(BotId, Result<MessagePage, CommandError>),
    /// The page before the message named, for that Bot.
    Older(BotId, MessageId, Result<MessagePage, CommandError>),
    Sent(BotId, Result<SendMessageResponse, CommandError>),
    Stopped(BotId, Result<StopResponse, CommandError>),
    Usage(Result<Usage, CommandError>),
    Created(Result<Bot, CommandError>),
    Account(Result<CodexAccount, CommandError>),
    Login(Result<DeviceCode, CommandError>),
}

/// A device-code sign-in as the gateway started it. The code is shown on
/// screen and nowhere else, so its `Debug` leaves it out.
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(transparent)]
pub struct DeviceCode(pub DeviceCodeLogin);

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCode")
            .field("verification_url", &self.0.verification_url)
            .field("user_code", &"<redacted>")
            .finish()
    }
}

impl ApiRequest {
    pub fn method(&self) -> Method {
        match self {
            Self::CheckSession
            | Self::ListBots
            | Self::LoadMessages(_)
            | Self::LoadOlder(..)
            | Self::LoadUsage
            | Self::CodexAccount => Method::Get,
            Self::SignIn(_)
            | Self::SendMessage(..)
            | Self::StopBot(_)
            | Self::CreateBot(_)
            | Self::CodexLogin => Method::Post,
            Self::SignOut => Method::Delete,
        }
    }

    /// The path, relative to the page's origin.
    pub fn path(&self) -> String {
        let route = match self {
            Self::CheckSession | Self::SignIn(_) | Self::SignOut => "/session".into(),
            Self::ListBots | Self::CreateBot(_) => "/bots".into(),
            Self::CodexAccount => "/codex/account".into(),
            Self::CodexLogin => "/codex/login".into(),
            Self::LoadMessages(bot) | Self::SendMessage(bot, _) => format!("/bots/{bot}/messages"),
            Self::LoadOlder(bot, before) => format!("/bots/{bot}/messages?before={before}"),
            Self::StopBot(bot) => format!("/bots/{bot}/stop"),
            Self::LoadUsage => "/usage".into(),
        };
        format!("{API_PREFIX}{route}")
    }

    /// The JSON body, if the command has one.
    pub fn body(&self) -> Option<String> {
        match self {
            Self::SignIn(login) => Some(json(login)),
            Self::SendMessage(_, message) => Some(json(message)),
            Self::CreateBot(bot) => Some(json(bot)),
            _ => None,
        }
    }

    /// Decode the response to this command.
    pub fn decode(&self, status: u16, body: &str) -> ApiReply {
        match self {
            Self::CheckSession => ApiReply::Session(wire::decode_response(status, body)),
            Self::SignIn(_) => ApiReply::SignedIn(wire::decode_response(status, body)),
            Self::SignOut => ApiReply::SignedOut(wire::decode_empty(status, body)),
            Self::ListBots => ApiReply::Bots(wire::decode_response(status, body)),
            Self::LoadMessages(bot) => {
                ApiReply::Messages(*bot, wire::decode_response(status, body))
            }
            Self::LoadOlder(bot, before) => {
                ApiReply::Older(*bot, *before, wire::decode_response(status, body))
            }
            Self::SendMessage(bot, _) => ApiReply::Sent(*bot, wire::decode_response(status, body)),
            Self::StopBot(bot) => ApiReply::Stopped(*bot, wire::decode_response(status, body)),
            Self::LoadUsage => ApiReply::Usage(wire::decode_response(status, body)),
            Self::CreateBot(_) => ApiReply::Created(wire::decode_response(status, body)),
            Self::CodexAccount => ApiReply::Account(wire::decode_response(status, body)),
            Self::CodexLogin => ApiReply::Login(wire::decode_response(status, body)),
        }
    }

    /// The reply when no response arrived at all.
    pub fn unreachable(&self) -> ApiReply {
        let error = CommandError::unreachable();
        match self {
            Self::CheckSession => ApiReply::Session(Err(error)),
            Self::SignIn(_) => ApiReply::SignedIn(Err(error)),
            Self::SignOut => ApiReply::SignedOut(Err(error)),
            Self::ListBots => ApiReply::Bots(Err(error)),
            Self::LoadMessages(bot) => ApiReply::Messages(*bot, Err(error)),
            Self::LoadOlder(bot, before) => ApiReply::Older(*bot, *before, Err(error)),
            Self::SendMessage(bot, _) => ApiReply::Sent(*bot, Err(error)),
            Self::StopBot(bot) => ApiReply::Stopped(*bot, Err(error)),
            Self::LoadUsage => ApiReply::Usage(Err(error)),
            Self::CreateBot(_) => ApiReply::Created(Err(error)),
            Self::CodexAccount => ApiReply::Account(Err(error)),
            Self::CodexLogin => ApiReply::Login(Err(error)),
        }
    }
}

fn json(body: &impl serde::Serialize) -> String {
    serde_json::to_string(body).expect("request bodies serialise")
}

/// Which sign-in a command belongs to. It changes whenever the user signs in
/// or out, so a reply that arrives after that, possibly with another user's
/// data, is recognised and dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Generation(pub u64);

/// Something that carries commands to the gateway and reports each reply
/// back to the application as an intent, tagged with the command's
/// generation. It also runs the event socket
/// ([`crate::socket::Driver`]), whose updates arrive as intents tagged the
/// same way.
pub trait Api {
    fn send(&self, generation: Generation, request: ApiRequest);

    /// Open the event socket for the sign-in `generation`, replacing any
    /// other, with a connection that has never been live.
    fn connect_events(&self, generation: Generation);

    /// Close the event socket for good, as on signing out.
    fn disconnect_events(&self);

    /// Reload the page, to fetch the client that matches the gateway.
    fn reload_page(&self);

    /// The time now, for saying how long until a limit resets.
    fn now(&self) -> Timestamp;
}

/// What the application asked of the event socket, as a [`Recorder`] keeps
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketControl {
    Connect(Generation),
    Disconnect,
    ReloadPage,
}

/// An [`Api`] that only records what it was asked to send, for tests and for
/// the native build, which has no network transport.
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    sent: std::rc::Rc<std::cell::RefCell<Vec<ApiRequest>>>,
    socket: std::rc::Rc<std::cell::RefCell<Vec<SocketControl>>>,
    now: std::rc::Rc<std::cell::Cell<i64>>,
}

impl Recorder {
    /// Every command sent so far, which is then forgotten.
    pub fn take(&self) -> Vec<ApiRequest> {
        std::mem::take(&mut self.sent.borrow_mut())
    }

    /// Set the time [`Api::now`] reports.
    pub fn set_now(&self, now: Timestamp) {
        self.now.set(now.0);
    }

    /// Everything asked of the event socket so far, which is then forgotten.
    pub fn take_socket(&self) -> Vec<SocketControl> {
        std::mem::take(&mut self.socket.borrow_mut())
    }
}

impl Api for Recorder {
    fn send(&self, _generation: Generation, request: ApiRequest) {
        self.sent.borrow_mut().push(request);
    }

    fn connect_events(&self, generation: Generation) {
        self.socket
            .borrow_mut()
            .push(SocketControl::Connect(generation));
    }

    fn disconnect_events(&self) {
        self.socket.borrow_mut().push(SocketControl::Disconnect);
    }

    fn reload_page(&self) {
        self.socket.borrow_mut().push(SocketControl::ReloadPage);
    }

    fn now(&self) -> Timestamp {
        Timestamp(self.now.get())
    }
}

#[cfg(test)]
mod tests {
    use bokkie_protocol::api::ErrorCode;

    use super::*;

    #[test]
    fn commands_name_their_routes() {
        let login = ApiRequest::SignIn(LoginRequest {
            username: "ada".into(),
            password: "a long password".into(),
        });
        let routes: Vec<(&str, String)> = [
            ApiRequest::CheckSession,
            login.clone(),
            ApiRequest::SignOut,
            ApiRequest::ListBots,
            ApiRequest::LoadMessages(BotId(3)),
            ApiRequest::LoadOlder(BotId(3), MessageId(41)),
            ApiRequest::SendMessage(
                BotId(3),
                SendMessageRequest {
                    text: "Hello".into(),
                },
            ),
            ApiRequest::StopBot(BotId(3)),
            ApiRequest::LoadUsage,
            ApiRequest::CreateBot(CreateBotRequest {
                name: "Gardener".into(),
                label: String::new(),
                description: String::new(),
                avatar: None,
            }),
            ApiRequest::CodexAccount,
            ApiRequest::CodexLogin,
        ]
        .iter()
        .map(|request| (request.method().as_str(), request.path()))
        .collect();
        assert_eq!(
            routes,
            [
                ("GET", "/api/v1/session".to_owned()),
                ("POST", "/api/v1/session".to_owned()),
                ("DELETE", "/api/v1/session".to_owned()),
                ("GET", "/api/v1/bots".to_owned()),
                ("GET", "/api/v1/bots/3/messages".to_owned()),
                ("GET", "/api/v1/bots/3/messages?before=41".to_owned()),
                ("POST", "/api/v1/bots/3/messages".to_owned()),
                ("POST", "/api/v1/bots/3/stop".to_owned()),
                ("GET", "/api/v1/usage".to_owned()),
                ("POST", "/api/v1/bots".to_owned()),
                ("GET", "/api/v1/codex/account".to_owned()),
                ("POST", "/api/v1/codex/login".to_owned()),
            ]
        );
        assert_eq!(
            ApiRequest::SendMessage(
                BotId(3),
                SendMessageRequest {
                    text: "Hello".into()
                }
            )
            .body()
            .as_deref(),
            Some(r#"{"text":"Hello"}"#)
        );
        assert_eq!(
            login.body().as_deref(),
            Some(r#"{"username":"ada","password":"a long password"}"#)
        );
        assert_eq!(ApiRequest::ListBots.body(), None);
        assert!(!format!("{login:?}").contains("a long password"));
    }

    #[test]
    fn a_new_bot_is_named_only_and_a_device_code_never_reaches_a_log() {
        let create = ApiRequest::CreateBot(CreateBotRequest {
            name: "Gardener".into(),
            label: String::new(),
            description: String::new(),
            avatar: None,
        });
        assert_eq!(
            create.body().as_deref(),
            Some(r#"{"name":"Gardener","label":"","description":""}"#)
        );
        let reply = ApiRequest::CodexLogin.decode(
            200,
            r#"{"verification_url":"https://auth.example/device","user_code":"WXYZ-1234","state":"pending","expires_at":1800000900000}"#,
        );
        let ApiReply::Login(Ok(login)) = &reply else {
            panic!("a device code");
        };
        assert_eq!(login.0.user_code, "WXYZ-1234");
        let logged = format!("{reply:?}");
        assert!(!logged.contains("WXYZ-1234"), "{logged}");
        assert!(logged.contains("https://auth.example/device"));
    }

    #[test]
    fn replies_decode_per_command() {
        let refused = r#"{"code":"unauthenticated","message":"The username or password is wrong"}"#;
        let ApiReply::SignedIn(Err(error)) = ApiRequest::SignIn(LoginRequest {
            username: "ada".into(),
            password: "wrong".into(),
        })
        .decode(401, refused) else {
            panic!("a refused sign-in");
        };
        assert_eq!(error.message, "The username or password is wrong");
        assert_eq!(
            ApiRequest::SignOut.decode(204, ""),
            ApiReply::SignedOut(Ok(()))
        );
        let ApiReply::Bots(Ok(list)) = ApiRequest::ListBots.decode(200, r#"{"bots":[],"seq":4}"#)
        else {
            panic!("a Bot list");
        };
        assert!(list.bots.is_empty());
        let ApiReply::Session(Err(error)) = ApiRequest::CheckSession.unreachable() else {
            panic!("no response");
        };
        assert_eq!((error.status, error.code), (0, ErrorCode::Unavailable));
    }
}
