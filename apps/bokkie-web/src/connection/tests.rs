use bokkie_protocol::Timestamp;
use bokkie_protocol::events::Event;

use super::*;

fn event(seq: u64) -> ServerFrame {
    ServerFrame::Event(envelope(seq))
}

fn envelope(seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq: Seq(seq),
        at: Timestamp(1_000),
        event: Event::BotError {
            bot_id: bokkie_protocol::BotId(1),
            message: format!("event {seq}"),
        },
    }
}

fn frame(connection: &mut Connection, frame: ServerFrame) -> Vec<Command> {
    connection.handle(Input::Frame(frame))
}

/// A connection that resumed after `last` and is live.
fn live(last: u64) -> Connection {
    let mut connection = Connection::new();
    let _ = connection.handle(Input::Start);
    let _ = connection.handle(Input::Opened);
    let _ = frame(&mut connection, ServerFrame::Live { latest: Seq(last) });
    assert_eq!(connection.state(), State::Live);
    connection
}

#[test]
fn the_first_connection_resumes_from_the_end_and_reloads_once_live() {
    let mut connection = Connection::new();
    assert_eq!(connection.state(), State::Stopped);
    assert_eq!(connection.handle(Input::Start), [Command::Connect]);
    assert_eq!(connection.state(), State::Connecting);
    assert_eq!(
        connection.handle(Input::Opened),
        [Command::Send(ClientFrame::Resume { after: None })]
    );
    assert_eq!(connection.state(), State::Resuming);
    assert_eq!(
        frame(&mut connection, ServerFrame::Live { latest: Seq(12) }),
        [Command::Reload]
    );
    assert_eq!(connection.state(), State::Live);
    assert_eq!(connection.last(), Some(Seq(12)));
    assert_eq!(
        frame(&mut connection, event(13)),
        [Command::Apply(envelope(13))]
    );
    assert_eq!(connection.last(), Some(Seq(13)));
}

#[test]
fn a_reconnection_resumes_after_the_last_event_and_replays() {
    let mut connection = live(12);
    let _ = frame(&mut connection, event(13));
    assert_eq!(
        connection.handle(Input::Closed { code: Some(1006) }),
        [Command::RetryAfter(FIRST_RETRY)]
    );
    assert!(matches!(
        connection.state(),
        State::BackingOff { attempt: 1, .. }
    ));
    assert_eq!(connection.handle(Input::RetryDue), [Command::Connect]);
    assert_eq!(
        connection.handle(Input::Opened),
        [Command::Send(ClientFrame::Resume {
            after: Some(Seq(13))
        })]
    );
    assert_eq!(
        frame(&mut connection, event(14)),
        [Command::Apply(envelope(14))]
    );
    assert_eq!(connection.state(), State::Replaying);
    assert_eq!(
        frame(&mut connection, event(15)),
        [Command::Apply(envelope(15))]
    );
    // Replay finished where it should: no reload.
    assert!(frame(&mut connection, ServerFrame::Live { latest: Seq(15) }).is_empty());
    assert_eq!(connection.state(), State::Live);
}

#[test]
fn the_delay_doubles_from_250_milliseconds_to_8_seconds_and_resets_when_live() {
    let mut connection = Connection::new();
    let _ = connection.handle(Input::Start);
    let mut delays = Vec::new();
    for _ in 0..8 {
        match connection.handle(Input::Closed { code: None }).as_slice() {
            [Command::RetryAfter(delay), Command::CheckSession] => delays.push(delay.as_millis()),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(connection.handle(Input::RetryDue), [Command::Connect]);
    }
    assert_eq!(delays, [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000, 8_000]);
    assert_eq!(Connection::delay(u32::MAX), LAST_RETRY);

    let _ = connection.handle(Input::Opened);
    let _ = frame(&mut connection, ServerFrame::Live { latest: Seq(0) });
    assert_eq!(
        connection.handle(Input::Closed { code: None }),
        [Command::RetryAfter(FIRST_RETRY)]
    );
}

#[test]
fn a_visible_page_retries_at_once() {
    let mut connection = live(3);
    let _ = connection.handle(Input::Closed { code: None });
    assert_eq!(
        connection.handle(Input::Visible),
        [Command::CancelRetry, Command::Connect]
    );
    assert_eq!(connection.state(), State::Connecting);
    // Already connecting: nothing more.
    assert!(connection.handle(Input::Visible).is_empty());
    // A retry timer that fires anyway changes nothing.
    assert!(connection.handle(Input::RetryDue).is_empty());
}

#[test]
fn a_visible_page_probes_a_live_socket() {
    let mut connection = live(3);
    assert_eq!(
        connection.handle(Input::Visible),
        [Command::Send(ClientFrame::Ping)]
    );
    assert!(frame(&mut connection, ServerFrame::Pong).is_empty());
    assert_eq!(connection.state(), State::Live);
}

#[test]
fn a_socket_that_fails_before_opening_checks_the_session() {
    let mut connection = Connection::new();
    let _ = connection.handle(Input::Start);
    assert_eq!(
        connection.handle(Input::Closed { code: None }),
        [Command::RetryAfter(FIRST_RETRY), Command::CheckSession]
    );
    // The check answered 401.
    assert_eq!(
        connection.handle(Input::Unauthenticated),
        [Command::Close, Command::CancelRetry, Command::SignedOut]
    );
    assert_eq!(connection.state(), State::SignedOut);
    for input in [Input::RetryDue, Input::Visible, Input::Start, Input::Opened] {
        assert!(
            connection.handle(input).is_empty(),
            "signed out stays signed out"
        );
    }
}

#[test]
fn the_gateway_ending_the_session_signs_out() {
    let mut connection = live(3);
    assert_eq!(
        connection.handle(Input::Closed {
            code: Some(CLOSE_SIGNED_OUT)
        }),
        [Command::SignedOut]
    );
    assert_eq!(connection.state(), State::SignedOut);
}

#[test]
fn a_401_from_any_request_signs_out() {
    for mut connection in [live(3), Connection::new()] {
        let _ = connection.handle(Input::Unauthenticated);
        assert_eq!(connection.state(), State::SignedOut);
    }
}

#[test]
fn duplicate_and_out_of_order_events_are_dropped() {
    let mut connection = live(10);
    assert!(
        frame(&mut connection, event(10)).is_empty(),
        "already processed"
    );
    assert_eq!(frame(&mut connection, event(11)).len(), 1);
    assert!(frame(&mut connection, event(11)).is_empty(), "a duplicate");
    assert!(frame(&mut connection, event(9)).is_empty(), "older");
    assert_eq!(
        frame(&mut connection, event(12)),
        [Command::Apply(envelope(12))]
    );
    assert_eq!(connection.last(), Some(Seq(12)));
}

#[test]
fn a_gap_in_the_log_resumes_from_the_last_event() {
    let mut connection = live(10);
    assert_eq!(
        frame(&mut connection, event(12)),
        [Command::Close, Command::RetryAfter(FIRST_RETRY)]
    );
    assert_eq!(connection.last(), Some(Seq(10)));
    let _ = connection.handle(Input::RetryDue);
    assert_eq!(
        connection.handle(Input::Opened),
        [Command::Send(ClientFrame::Resume {
            after: Some(Seq(10))
        })]
    );
}

#[test]
fn a_replay_that_ends_short_resumes_again() {
    let mut connection = live(10);
    let _ = connection.handle(Input::Closed { code: None });
    let _ = connection.handle(Input::RetryDue);
    let _ = connection.handle(Input::Opened);
    let _ = frame(&mut connection, event(11));
    assert_eq!(
        frame(&mut connection, ServerFrame::Live { latest: Seq(13) }),
        // The second failure in a row: the delay has doubled.
        [Command::Close, Command::RetryAfter(Connection::delay(2))]
    );
    assert_eq!(connection.last(), Some(Seq(11)));
}

#[test]
fn a_log_behind_the_client_reloads() {
    let mut connection = live(10);
    let _ = connection.handle(Input::Closed { code: None });
    let _ = connection.handle(Input::RetryDue);
    let _ = connection.handle(Input::Opened);
    assert_eq!(
        frame(&mut connection, ServerFrame::Live { latest: Seq(4) }),
        [Command::Reload]
    );
    assert_eq!(connection.last(), Some(Seq(4)));
}

#[test]
fn reset_reloads_and_continues_after_it() {
    let mut connection = live(10);
    let _ = connection.handle(Input::Closed { code: None });
    let _ = connection.handle(Input::RetryDue);
    let _ = connection.handle(Input::Opened);
    assert_eq!(
        frame(&mut connection, ServerFrame::Reset { after: Seq(7_000) }),
        [Command::Reload]
    );
    assert_eq!(connection.state(), State::Replaying);
    assert_eq!(connection.last(), Some(Seq(7_000)));
    assert!(frame(&mut connection, ServerFrame::Live { latest: Seq(7_000) }).is_empty());
    assert_eq!(
        frame(&mut connection, event(7_001)),
        [Command::Apply(envelope(7_001))]
    );
}

#[test]
fn an_unreadable_frame_resumes_from_the_last_event() {
    let mut connection = live(10);
    assert_eq!(
        connection.handle(Input::Unreadable),
        [Command::Close, Command::RetryAfter(FIRST_RETRY)]
    );
    assert_eq!(connection.last(), Some(Seq(10)));
}

#[test]
fn stop_closes_and_stays_stopped_until_started() {
    let mut connection = live(10);
    assert_eq!(
        connection.handle(Input::Stop),
        [Command::Close, Command::CancelRetry]
    );
    assert_eq!(connection.state(), State::Stopped);
    assert!(connection.handle(Input::Closed { code: None }).is_empty());
    assert!(frame(&mut connection, event(11)).is_empty());
    assert_eq!(connection.handle(Input::Start), [Command::Connect]);
    assert_eq!(
        connection.handle(Input::Opened),
        [Command::Send(ClientFrame::Resume {
            after: Some(Seq(10))
        })]
    );
}

#[test]
fn late_callbacks_from_an_abandoned_socket_are_ignored() {
    let mut connection = live(10);
    let _ = connection.handle(Input::Closed { code: None });
    assert!(connection.handle(Input::Opened).is_empty());
    assert!(frame(&mut connection, event(11)).is_empty());
    assert!(matches!(connection.state(), State::BackingOff { .. }));
}
