//! The HTTP commands the client sends, independent of how they travel.
//!
//! Each [`ApiRequest`] knows its method, path and body, and how to decode
//! its response into an [`ApiReply`]. The browser transport
//! (`crate::browser`) sends them with `fetch` and runs the event socket;
//! tests record them and reply by hand.

use bokkie_protocol::api::{
    BotList, LoginRequest, MessagePage, SendMessageRequest, SendMessageResponse, SessionResponse,
};
use bokkie_protocol::{API_PREFIX, BotId};

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
    /// `POST /api/v1/bots/{id}/messages`: send a message, which starts a
    /// turn or steers the running one.
    SendMessage(BotId, SendMessageRequest),
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
            Self::SendMessage(bot, message) => f
                .debug_tuple("SendMessage")
                .field(bot)
                .field(message)
                .finish(),
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
    Sent(BotId, Result<SendMessageResponse, CommandError>),
}

impl ApiRequest {
    pub fn method(&self) -> Method {
        match self {
            Self::CheckSession | Self::ListBots | Self::LoadMessages(_) => Method::Get,
            Self::SignIn(_) | Self::SendMessage(..) => Method::Post,
            Self::SignOut => Method::Delete,
        }
    }

    /// The path, relative to the page's origin.
    pub fn path(&self) -> String {
        let route = match self {
            Self::CheckSession | Self::SignIn(_) | Self::SignOut => "/session".into(),
            Self::ListBots => "/bots".into(),
            Self::LoadMessages(bot) | Self::SendMessage(bot, _) => format!("/bots/{bot}/messages"),
        };
        format!("{API_PREFIX}{route}")
    }

    /// The JSON body, if the command has one.
    pub fn body(&self) -> Option<String> {
        match self {
            Self::SignIn(login) => Some(json(login)),
            Self::SendMessage(_, message) => Some(json(message)),
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
            Self::SendMessage(bot, _) => ApiReply::Sent(*bot, wire::decode_response(status, body)),
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
            Self::SendMessage(bot, _) => ApiReply::Sent(*bot, Err(error)),
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
}

impl Recorder {
    /// Every command sent so far, which is then forgotten.
    pub fn take(&self) -> Vec<ApiRequest> {
        std::mem::take(&mut self.sent.borrow_mut())
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
            ApiRequest::SendMessage(
                BotId(3),
                SendMessageRequest {
                    text: "Hello".into(),
                },
            ),
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
                ("POST", "/api/v1/bots/3/messages".to_owned()),
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
