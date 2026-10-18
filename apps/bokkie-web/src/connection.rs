//! The event WebSocket's connection policy, as a state machine with no I/O.
//!
//! The transport reports what happened ([`Input`]) and carries out what the
//! machine asks for ([`Command`]). The machine owns the position in the
//! user's event log: the last sequence number it passed on. Each connection
//! resumes from there, so a dropped socket loses no event and repeats none.
//!
//! - **Connecting.** The transport opens the socket.
//! - **Resuming.** The socket is open and [`ClientFrame::Resume`] names the
//!   last sequence number processed (`None` on the first connection).
//! - **Replaying.** Events after that number arrive, until
//!   [`ServerFrame::Live`].
//! - **Live.** Events arrive as they happen.
//! - **Backing off.** The socket closed or failed. The machine waits 250 ms,
//!   doubling to 8 s, before connecting again. A page that becomes visible
//!   again connects at once. Reaching live resets the delay.
//! - **Signed out.** A request answered 401, or the gateway closed the socket
//!   because the session ended. Nothing reconnects.
//!
//! On the first connection, and whenever the gateway sends
//! [`ServerFrame::Reset`], the client must reload its snapshots over HTTP
//! ([`Command::Reload`]). On the first connection it does so once live, so
//! that the snapshots are read after the gateway subscribed this socket to
//! the log, and no event can fall between the two.

use std::time::Duration;

use bokkie_protocol::events::{ClientFrame, EventEnvelope, Seq, ServerFrame};

/// The first reconnection delay.
pub const FIRST_RETRY: Duration = Duration::from_millis(250);
/// The longest reconnection delay.
pub const LAST_RETRY: Duration = Duration::from_secs(8);

/// The close code the gateway sends when the socket's session ends.
pub const CLOSE_SIGNED_OUT: u16 = 4002;

/// Where the connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not started, or stopped.
    Stopped,
    Connecting,
    Resuming,
    Replaying,
    Live,
    /// Waiting before connection attempt `attempt`.
    BackingOff {
        attempt: u32,
        delay: Duration,
    },
    SignedOut,
}

/// What happened, as the transport or the application reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Start connecting, after sign-in or when the application starts signed
    /// in.
    Start,
    /// The socket opened.
    Opened,
    /// A frame arrived.
    Frame(ServerFrame),
    /// A text message arrived that is not a frame this client reads.
    Unreadable,
    /// The socket closed or failed, with the close code if the gateway sent
    /// one.
    Closed { code: Option<u16> },
    /// The reconnection delay elapsed.
    RetryDue,
    /// The page became visible again.
    Visible,
    /// An HTTP request answered 401: the session is gone.
    Unauthenticated,
    /// Stop for good, as on sign-out.
    Stop,
}

/// What the transport or the application should do now, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open the socket.
    Connect,
    /// Send a frame.
    Send(ClientFrame),
    /// Close the socket.
    Close,
    /// Report [`Input::RetryDue`] after this delay.
    RetryAfter(Duration),
    /// Forget the pending [`Command::RetryAfter`].
    CancelRetry,
    /// Apply this event to the store.
    Apply(EventEnvelope),
    /// Reload every snapshot over HTTP (see [`crate::store::Store::reload_all`]).
    Reload,
    /// The socket failed before it opened, which is how a browser reports a
    /// refused upgrade. Check the session over HTTP, and report
    /// [`Input::Unauthenticated`] if it is gone.
    CheckSession,
    /// The session is gone: show the signed-out screen.
    SignedOut,
}

/// The reconnecting event connection.
#[derive(Debug, Clone)]
pub struct Connection {
    state: State,
    /// Failed attempts since the connection was last live.
    failures: u32,
    /// The last sequence number passed on, if any.
    last: Option<Seq>,
    /// Whether the snapshots have been asked for since this user signed in.
    synced: bool,
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

impl Connection {
    /// A connection that has never been live, for a newly signed-in user.
    pub fn new() -> Self {
        Self {
            state: State::Stopped,
            failures: 0,
            last: None,
            synced: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The last sequence number passed on, which the next connection resumes
    /// after.
    pub fn last(&self) -> Option<Seq> {
        self.last
    }

    /// The delay before attempt `attempt` (1 for the first retry): 250 ms,
    /// doubling to 8 s.
    pub fn delay(attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(6);
        (FIRST_RETRY * 2u32.pow(doublings)).min(LAST_RETRY)
    }

    /// Handle one input and return what to do.
    pub fn handle(&mut self, input: Input) -> Vec<Command> {
        use State::*;
        match (self.state, input) {
            (SignedOut, _) => Vec::new(),
            (_, Input::Unauthenticated) => {
                self.state = SignedOut;
                vec![Command::Close, Command::CancelRetry, Command::SignedOut]
            }
            (_, Input::Stop) => {
                self.state = Stopped;
                vec![Command::Close, Command::CancelRetry]
            }
            (Stopped, Input::Start) => self.connect(),
            (BackingOff { .. }, Input::RetryDue) => self.connect(),
            (BackingOff { .. }, Input::Visible) => {
                let mut commands = vec![Command::CancelRetry];
                commands.extend(self.connect());
                commands
            }
            // A socket that may have died while the page was hidden answers
            // a ping, or fails and closes.
            (Live, Input::Visible) => vec![Command::Send(ClientFrame::Ping)],
            (Connecting, Input::Opened) => {
                self.state = Resuming;
                vec![Command::Send(ClientFrame::Resume { after: self.last })]
            }
            (Connecting, Input::Closed { code }) => {
                let mut commands = self.closed(code);
                if self.state != SignedOut {
                    commands.push(Command::CheckSession);
                }
                commands
            }
            (Resuming | Replaying | Live, Input::Closed { code }) => self.closed(code),
            (Resuming | Replaying | Live, Input::Frame(frame)) => self.frame(frame),
            // A frame the gateway would never send means the two disagree;
            // start again from the last event processed.
            (Resuming | Replaying | Live, Input::Unreadable) => self.resync(),
            // Late callbacks from a socket already given up, or a repeated
            // start: nothing to do.
            _ => Vec::new(),
        }
    }

    fn connect(&mut self) -> Vec<Command> {
        self.state = State::Connecting;
        vec![Command::Connect]
    }

    fn closed(&mut self, code: Option<u16>) -> Vec<Command> {
        if code == Some(CLOSE_SIGNED_OUT) {
            self.state = State::SignedOut;
            return vec![Command::SignedOut];
        }
        self.failures = self.failures.saturating_add(1);
        let delay = Self::delay(self.failures);
        self.state = State::BackingOff {
            attempt: self.failures,
            delay,
        };
        vec![Command::RetryAfter(delay)]
    }

    /// Give up on this socket and back off, as if it had closed.
    fn resync(&mut self) -> Vec<Command> {
        let mut commands = vec![Command::Close];
        commands.extend(self.closed(None));
        commands
    }

    fn frame(&mut self, frame: ServerFrame) -> Vec<Command> {
        match frame {
            ServerFrame::Event(envelope) => {
                match self.last {
                    // Already passed on: a duplicate or an older event.
                    Some(last) if envelope.seq <= last => return Vec::new(),
                    // The log has no gaps, so a missing event means this
                    // socket cannot be trusted. Resume from the last one.
                    Some(last) if envelope.seq != last.next() => return self.resync(),
                    _ => {}
                }
                self.last = Some(envelope.seq);
                if self.state == State::Resuming {
                    self.state = State::Replaying;
                }
                vec![Command::Apply(envelope)]
            }
            ServerFrame::Live { latest } => {
                if self.state == State::Live {
                    return Vec::new();
                }
                match self.last {
                    // Events up to `latest` should all have been replayed.
                    Some(last) if last < latest => return self.resync(),
                    _ => {}
                }
                // A log behind the client's position has been replaced;
                // what the client holds may not exist any more.
                let replaced = self.last.is_some_and(|last| last > latest);
                self.last = Some(latest);
                self.state = State::Live;
                self.failures = 0;
                if !self.synced || replaced {
                    self.synced = true;
                    vec![Command::Reload]
                } else {
                    Vec::new()
                }
            }
            ServerFrame::Reset { after } => {
                self.last = Some(after);
                self.synced = true;
                if self.state == State::Resuming {
                    self.state = State::Replaying;
                }
                vec![Command::Reload]
            }
            ServerFrame::Pong => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
