//! The event socket's driver: the [`Connection`] policy, with its commands
//! split between the transport's I/O and what the application must hear.
//!
//! The browser transport owns one [`Driver`]. It reports what the socket did
//! as an [`Input`] (or a received text message, through [`Driver::receive`]),
//! performs the returned [`Io`], and sends each [`SocketUpdate`] to the
//! application as an intent. Only updates wake the application, so pongs,
//! duplicates and already-applied replays, which the connection drops, cost
//! it no pass, and the link status is reported only when it changes.
//!
//! This is platform-independent, so host tests drive the application through
//! a driver exactly as the browser does, with frames in place of a socket.

use std::time::Duration;

use bokkie_protocol::events::EventEnvelope;

use crate::connection::{Command, Connection, Input, State};
use crate::wire;

/// What the transport does to the socket and its timers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Io {
    /// Open the socket to the event route, replacing any other.
    Connect,
    /// Send this text message.
    Send(String),
    /// Close the socket and forget it.
    Close,
    /// Report [`Input::RetryDue`] after this delay.
    RetryAfter(Duration),
    /// Forget the pending retry.
    CancelRetry,
    /// Report [`Input::PongOverdue`] for `ping` after this delay.
    AwaitPong { ping: u32, after: Duration },
    /// Report [`Input::ViewingDue`] for `renewal` after this delay.
    RenewViewing { renewal: u32, after: Duration },
}

/// What the application hears from the event socket.
// Updates are applied one at a time as they arrive, so the size of the
// event variant costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketUpdate {
    /// Apply this event from the log.
    Event(EventEnvelope),
    /// Reload every snapshot over HTTP.
    Reload,
    /// Check over HTTP whether the session still holds; it answers 401 if
    /// not.
    CheckSession,
    /// The gateway ended the session.
    SignedOut,
    /// The link's state changed.
    Link(Link),
}

/// The link's state as the application shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Link {
    /// Not started, or stopped.
    #[default]
    Stopped,
    /// Connecting for the first time since sign-in.
    Connecting,
    /// Live: events arrive as they happen.
    Live,
    /// Was live; connecting again, or catching up.
    Reconnecting,
    SignedOut,
    /// The gateway speaks a protocol this client cannot read.
    Outdated,
}

impl Link {
    /// The name the `connection` fact uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Connecting => "connecting",
            Self::Live => "live",
            Self::Reconnecting => "reconnecting",
            Self::SignedOut => "signed_out",
            Self::Outdated => "outdated",
        }
    }
}

/// What one input produced: I/O for the transport, then updates for the
/// application, each in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct Step {
    pub io: Vec<Io>,
    pub updates: Vec<SocketUpdate>,
}

/// One sign-in's event connection.
#[derive(Debug, Clone, Default)]
pub struct Driver {
    connection: Connection,
    link: Link,
    /// Whether the link has been live since it started.
    was_live: bool,
}

impl Driver {
    /// A driver for a newly signed-in user, which has never connected.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn link(&self) -> Link {
        self.link
    }

    /// A text message from the socket.
    pub fn receive(&mut self, text: &str) -> Step {
        match wire::decode_frame(text) {
            Ok(frame) => self.handle(Input::Frame(frame)),
            Err(_) => self.handle(Input::Unreadable),
        }
    }

    /// Handle one input.
    pub fn handle(&mut self, input: Input) -> Step {
        let mut step = Step::default();
        for command in self.connection.handle(input) {
            match command {
                Command::Connect => step.io.push(Io::Connect),
                Command::Send(frame) => step.io.push(Io::Send(wire::encode_frame(&frame))),
                Command::Close => step.io.push(Io::Close),
                Command::RetryAfter(delay) => step.io.push(Io::RetryAfter(delay)),
                Command::CancelRetry => step.io.push(Io::CancelRetry),
                Command::AwaitPong { ping, after } => step.io.push(Io::AwaitPong { ping, after }),
                Command::RenewViewing { renewal, after } => {
                    step.io.push(Io::RenewViewing { renewal, after })
                }
                Command::Apply(envelope) => step.updates.push(SocketUpdate::Event(*envelope)),
                Command::Reload => step.updates.push(SocketUpdate::Reload),
                Command::CheckSession => step.updates.push(SocketUpdate::CheckSession),
                Command::SignedOut => step.updates.push(SocketUpdate::SignedOut),
                // The link state below says so.
                Command::Outdated => {}
            }
        }
        let link = match self.connection.state() {
            State::Stopped => Link::Stopped,
            State::Live => Link::Live,
            State::SignedOut => Link::SignedOut,
            State::Outdated => Link::Outdated,
            State::Connecting | State::Resuming | State::Replaying | State::BackingOff { .. } => {
                if self.was_live {
                    Link::Reconnecting
                } else {
                    Link::Connecting
                }
            }
        };
        self.was_live |= link == Link::Live;
        if link != self.link {
            self.link = link;
            // After the events: the link turns live once the replay before
            // it has been applied.
            step.updates.push(SocketUpdate::Link(link));
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use bokkie_protocol::events::{ClientFrame, Event, Seq, ServerFrame};
    use bokkie_protocol::{BotId, Timestamp};

    use super::*;

    fn text(frame: &ServerFrame) -> String {
        serde_json::to_string(frame).unwrap()
    }

    fn event(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq: Seq(seq),
            at: Timestamp(1),
            event: Event::BotError {
                bot_id: BotId(1),
                message: format!("event {seq}"),
            },
        }
    }

    #[test]
    fn a_first_connection_reports_its_link_and_asks_for_a_reload_once_live() {
        let mut driver = Driver::new();
        let step = driver.handle(Input::Start);
        assert_eq!(step.io, [Io::Connect]);
        assert_eq!(step.updates, [SocketUpdate::Link(Link::Connecting)]);
        let step = driver.handle(Input::Opened);
        assert_eq!(
            step.io,
            [Io::Send(wire::encode_frame(&ClientFrame::Resume {
                after: None
            }))]
        );
        assert!(step.updates.is_empty(), "no change to report");
        let step = driver.receive(&text(&ServerFrame::Live { latest: Seq(4) }));
        assert_eq!(
            step.updates,
            [SocketUpdate::Reload, SocketUpdate::Link(Link::Live)]
        );
        let step = driver.receive(&text(&ServerFrame::Event(event(5))));
        assert_eq!(step.updates, [SocketUpdate::Event(event(5))]);
    }

    #[test]
    fn the_bot_shown_goes_out_as_viewing_and_is_renewed_by_timer() {
        let mut driver = Driver::new();
        let _ = driver.handle(Input::Viewing(Some(BotId(7))));
        let _ = driver.handle(Input::Start);
        let _ = driver.handle(Input::Opened);
        let step = driver.receive(&text(&ServerFrame::Live { latest: Seq(4) }));
        let viewing = Io::Send(wire::encode_frame(&ClientFrame::Viewing {
            bot_id: Some(BotId(7)),
        }));
        assert_eq!(
            step.io,
            [
                viewing.clone(),
                Io::RenewViewing {
                    renewal: 1,
                    after: crate::connection::VIEWING_RENEW
                }
            ]
        );
        assert_eq!(
            driver.handle(Input::ViewingDue { renewal: 1 }).io,
            [
                viewing,
                Io::RenewViewing {
                    renewal: 2,
                    after: crate::connection::VIEWING_RENEW
                }
            ]
        );
    }

    #[test]
    fn pongs_duplicates_and_replayed_events_produce_no_update() {
        let mut driver = Driver::new();
        let _ = driver.handle(Input::Start);
        let _ = driver.handle(Input::Opened);
        let _ = driver.receive(&text(&ServerFrame::Live { latest: Seq(4) }));
        let _ = driver.receive(&text(&ServerFrame::Event(event(5))));
        for frame in [
            ServerFrame::Pong,
            ServerFrame::Event(event(5)),
            ServerFrame::Event(event(3)),
            ServerFrame::Live { latest: Seq(5) },
        ] {
            assert_eq!(driver.receive(&text(&frame)), Step::default(), "{frame:?}");
        }
    }

    #[test]
    fn a_dropped_socket_reports_reconnecting_until_live_again() {
        let mut driver = Driver::new();
        let _ = driver.handle(Input::Start);
        let _ = driver.handle(Input::Opened);
        let _ = driver.receive(&text(&ServerFrame::Live { latest: Seq(4) }));
        let step = driver.handle(Input::Closed { code: Some(1006) });
        assert_eq!(step.io, [Io::RetryAfter(crate::connection::FIRST_RETRY)]);
        assert_eq!(step.updates, [SocketUpdate::Link(Link::Reconnecting)]);
        let step = driver.handle(Input::RetryDue);
        assert!(step.updates.is_empty());
        let _ = driver.handle(Input::Opened);
        let step = driver.receive(&text(&ServerFrame::Event(event(5))));
        assert_eq!(step.updates, [SocketUpdate::Event(event(5))]);
        let step = driver.receive(&text(&ServerFrame::Live { latest: Seq(5) }));
        assert_eq!(step.updates, [SocketUpdate::Link(Link::Live)]);
    }

    #[test]
    fn unreadable_text_resumes_and_then_reports_an_outdated_gateway() {
        let mut driver = Driver::new();
        let _ = driver.handle(Input::Start);
        let _ = driver.handle(Input::Opened);
        let _ = driver.receive(&text(&ServerFrame::Live { latest: Seq(0) }));
        let mut last = Step::default();
        for _ in 0..crate::connection::UNREADABLE_LIMIT {
            last = driver.receive(r#"{"type":"from_the_future"}"#);
            let _ = driver.handle(Input::RetryDue);
            let _ = driver.handle(Input::Opened);
        }
        assert_eq!(last.io, [Io::Close]);
        assert_eq!(last.updates, [SocketUpdate::Link(Link::Outdated)]);
        assert_eq!(driver.link(), Link::Outdated);
    }

    #[test]
    fn the_gateway_closing_for_a_revoked_session_signs_out() {
        let mut driver = Driver::new();
        let _ = driver.handle(Input::Start);
        let _ = driver.handle(Input::Opened);
        let _ = driver.receive(&text(&ServerFrame::Live { latest: Seq(0) }));
        let step = driver.handle(Input::Closed {
            code: Some(crate::connection::CLOSE_SIGNED_OUT),
        });
        assert!(step.io.is_empty());
        assert_eq!(
            step.updates,
            [SocketUpdate::SignedOut, SocketUpdate::Link(Link::SignedOut)]
        );
    }
}
