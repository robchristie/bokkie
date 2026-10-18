use bokkie_protocol::Timestamp;
use bokkie_protocol::api::{Allowance, Author, BotStatus, Limits, RateLimitBucket};

use super::*;

const BOT: BotId = BotId(1);

fn bot(id: i64, name: &str) -> Bot {
    Bot {
        id: BotId(id),
        name: name.into(),
        label: "Helper".into(),
        description: String::new(),
        avatar: None,
        pinned: false,
        hidden: false,
        status: BotStatus::Idle,
        created_at: Timestamp(1_000),
        updated_at: Timestamp(1_000),
        last_message: None,
    }
}

fn message(id: i64, author: Author, text: &str, state: MessageState) -> Message {
    Message {
        id: MessageId(id),
        bot_id: BOT,
        author,
        text: text.into(),
        state,
        created_at: Timestamp(2_000 + id),
        updated_at: Timestamp(2_000 + id),
    }
}

fn reply(id: i64, text: &str) -> Message {
    message(id, Author::Bot, text, MessageState::Streaming)
}

fn at(seq: u64, event: Event) -> EventEnvelope {
    EventEnvelope {
        seq: Seq(seq),
        at: Timestamp(3_000 + seq as i64),
        event,
    }
}

fn delta(seq: u64, message: i64, offset: u64, text: &str) -> EventEnvelope {
    at(
        seq,
        Event::MessageDelta {
            bot_id: BOT,
            message_id: MessageId(message),
            offset,
            delta: text.into(),
        },
    )
}

fn created(seq: u64, message: Message) -> EventEnvelope {
    at(seq, Event::MessageCreated { message })
}

fn completed(seq: u64, message: Message) -> EventEnvelope {
    at(seq, Event::MessageCompleted { message })
}

fn status(seq: u64, bot: i64, status: BotStatus) -> EventEnvelope {
    at(
        seq,
        Event::BotStatusChanged {
            bot_id: BotId(bot),
            status,
        },
    )
}

fn bots(seq: u64, bots: Vec<Bot>) -> BotList {
    BotList {
        bots,
        seq: Seq(seq),
    }
}

fn page(seq: u64, messages: Vec<Message>) -> MessagePage {
    MessagePage {
        messages,
        older: None,
        seq: Seq(seq),
    }
}

/// A store with Bot 1 listed at `seq` and its conversation loaded at `seq`.
fn open(seq: u64, messages: Vec<Message>) -> Store {
    let mut store = Store::new();
    assert!(store.request(Snapshot::Bots));
    let _ = store.load_bots(bots(seq, vec![bot(1, "Ada")]));
    assert!(store.request(Snapshot::Messages(BOT)));
    let _ = store.load_messages(BOT, page(seq, messages));
    store
}

fn text(store: &Store, id: i64) -> &str {
    &store
        .conversation(BOT)
        .unwrap()
        .message(MessageId(id))
        .unwrap()
        .text
}

#[test]
fn only_events_after_a_snapshot_apply_to_it() {
    let mut store = Store::new();
    assert!(store.request(Snapshot::Bots));
    let _ = store.load_bots(bots(5, vec![bot(1, "Ada")]));
    // Replayed from before the snapshot, which already reflects it.
    assert!(!store.apply(&status(5, 1, BotStatus::Working)).changed);
    assert_eq!(store.bot(BOT).unwrap().status, BotStatus::Idle);
    assert!(store.apply(&status(6, 1, BotStatus::Working)).changed);
    assert_eq!(store.bot(BOT).unwrap().status, BotStatus::Working);
    let renamed = Bot {
        name: "Ada Lovelace".into(),
        ..bot(1, "Ada")
    };
    assert!(
        store
            .apply(&at(7, Event::BotUpdated { bot: renamed }))
            .changed
    );
    assert_eq!(store.bot(BOT).unwrap().name, "Ada Lovelace");
    assert!(
        store
            .apply(&at(
                8,
                Event::BotCreated {
                    bot: bot(2, "Grace")
                }
            ))
            .changed
    );
    assert_eq!(store.visible_bots().len(), 2);
}

#[test]
fn events_that_arrive_while_a_snapshot_loads_are_replayed_onto_it() {
    let mut store = Store::new();
    assert!(store.request(Snapshot::Bots));
    // The socket is live before the list arrives.
    let _ = store.apply(&status(4, 1, BotStatus::Working));
    let _ = store.apply(&status(5, 1, BotStatus::Idle));
    let _ = store.apply(&at(
        6,
        Event::BotCreated {
            bot: bot(2, "Grace"),
        },
    ));
    assert!(!store.bots_loaded());
    // The list reflects event 4 but not 5 or 6.
    let mut working = bot(1, "Ada");
    working.status = BotStatus::Working;
    let applied = store.load_bots(bots(4, vec![working]));
    assert!(applied.changed && applied.reload.is_empty());
    assert_eq!(store.bot(BOT).unwrap().status, BotStatus::Idle);
    assert!(store.bot(BotId(2)).is_some());
    // Nothing is loading, so nothing more is held.
    assert!(store.held.is_empty());
}

#[test]
fn deltas_assemble_by_character_offset_without_duplicating_text() {
    let mut store = open(10, vec![reply(7, "")]);
    assert!(store.apply(&delta(11, 7, 0, "Héllo")).changed);
    // An overlapping delta adds only what is new.
    assert!(store.apply(&delta(12, 7, 3, "lo, wörld")).changed);
    assert_eq!(text(&store, 7), "Héllo, wörld");
    // A delta held in full changes nothing.
    assert!(!store.apply(&delta(13, 7, 0, "Héllo, wö")).changed);
    // Offsets count characters, not bytes: an emoji is one.
    assert!(store.apply(&delta(14, 7, 12, " 🐐!")).changed);
    assert_eq!(text(&store, 7), "Héllo, wörld 🐐!");
    assert!(store.apply(&delta(15, 7, 15, "?")).changed);
    assert_eq!(text(&store, 7), "Héllo, wörld 🐐!?");
}

#[test]
fn a_delta_beyond_the_text_held_reloads_the_conversation() {
    let mut store = open(10, vec![reply(7, "Hello")]);
    let applied = store.apply(&delta(11, 7, 9, "lost"));
    assert_eq!(applied.reload, BTreeSet::from([Snapshot::Messages(BOT)]));
    assert_eq!(text(&store, 7), "Hello", "nothing is guessed");
    // While the reload is under way it is not asked for again.
    assert!(store.apply(&delta(12, 7, 16, "more")).reload.is_empty());
    // The reloaded page reflects event 11; event 12 is replayed onto it.
    let applied = store.load_messages(BOT, page(11, vec![reply(7, "Hello, I am lost")]));
    assert!(applied.reload.is_empty());
    assert_eq!(text(&store, 7), "Hello, I am lostmore");
}

#[test]
fn a_delta_for_a_message_the_conversation_lacks_reloads_it() {
    let mut store = open(10, Vec::new());
    let applied = store.apply(&delta(11, 7, 0, "Hi"));
    assert_eq!(applied.reload, BTreeSet::from([Snapshot::Messages(BOT)]));
}

#[test]
fn completion_replaces_assembled_text_and_ends_the_stream() {
    let mut store = open(10, vec![reply(7, "")]);
    let _ = store.apply(&delta(11, 7, 0, "Draft tex"));
    let done = message(7, Author::Bot, "Final text", MessageState::Complete);
    assert!(store.apply(&completed(12, done.clone())).changed);
    assert_eq!(
        store.conversation(BOT).unwrap().message(MessageId(7)),
        Some(&done)
    );
    // A late delta cannot change a finished message.
    assert!(!store.apply(&delta(13, 7, 9, "t and more")).changed);
    assert_eq!(text(&store, 7), "Final text");
}

#[test]
fn a_replay_overlapping_a_snapshot_neither_loses_nor_repeats_text() {
    // The page says it reflects event 10, but it already shows text from
    // the deltas at 12 and 13, which the socket then replays.
    let mut store = open(10, vec![reply(7, "One two three")]);
    let _ = store.apply(&created(11, reply(7, "")));
    assert_eq!(text(&store, 7), "One two three", "a snapshot ahead is kept");
    let _ = store.apply(&delta(12, 7, 0, "One two"));
    let _ = store.apply(&delta(13, 7, 7, " three"));
    let _ = store.apply(&delta(14, 7, 13, " four"));
    assert_eq!(text(&store, 7), "One two three four");

    // A snapshot that already shows the reply finished keeps it finished.
    let done = message(8, Author::Bot, "Done", MessageState::Complete);
    let mut store = open(10, vec![done.clone()]);
    let _ = store.apply(&created(11, reply(8, "")));
    let _ = store.apply(&delta(12, 8, 0, "Do"));
    assert_eq!(
        store.conversation(BOT).unwrap().message(MessageId(8)),
        Some(&done)
    );
}

#[test]
fn applying_the_same_events_twice_changes_nothing() {
    let events = [
        created(11, reply(7, "")),
        delta(12, 7, 0, "Hello"),
        status(13, 1, BotStatus::Working),
        completed(
            14,
            message(7, Author::Bot, "Hello!", MessageState::Complete),
        ),
    ];
    let mut store = open(10, Vec::new());
    for event in &events {
        let _ = store.apply(event);
    }
    let conversation = store.conversation(BOT).unwrap().clone();
    let listed = store.bot(BOT).unwrap().clone();
    for event in &events {
        assert!(!store.apply(event).changed, "{event:?}");
    }
    assert_eq!(store.conversation(BOT).unwrap(), &conversation);
    assert_eq!(store.bot(BOT).unwrap(), &listed);
}

#[test]
fn previews_follow_the_newest_message() {
    let mut store = open(
        10,
        vec![message(5, Author::User, "Hi", MessageState::Complete)],
    );
    let _ = store.apply(&created(11, reply(6, "")));
    let _ = store.apply(&delta(12, 6, 0, "Thinking"));
    let preview = store.bot(BOT).unwrap().last_message.clone().unwrap();
    assert_eq!(
        (preview.author, preview.text.as_str()),
        (Author::Bot, "Thinking")
    );
    // A message sent while the reply streams is newer; the reply's
    // completion must not hide it.
    let steer = message(7, Author::User, "Shorter please", MessageState::Complete);
    let _ = store.apply(&created(13, steer));
    let _ = store.apply(&completed(
        14,
        message(6, Author::Bot, "Long answer", MessageState::Complete),
    ));
    let preview = store.bot(BOT).unwrap().last_message.clone().unwrap();
    assert_eq!(preview.text, "Shorter please");
    // Previews are as long as the gateway's.
    let long = "x".repeat(PREVIEW_CHARS + 10);
    let _ = store.apply(&created(
        15,
        message(8, Author::User, &long, MessageState::Complete),
    ));
    let preview = store.bot(BOT).unwrap().last_message.clone().unwrap();
    assert_eq!(preview.text.chars().count(), PREVIEW_CHARS);
}

#[test]
fn a_completion_the_list_cannot_place_reloads_the_list_once() {
    let mut store = Store::new();
    assert!(store.request(Snapshot::Bots));
    let mut listed = bot(1, "Ada");
    listed.last_message = Some(MessagePreview {
        author: Author::Bot,
        text: "Part".into(),
        at: Timestamp(2_007),
    });
    let _ = store.load_bots(bots(10, vec![listed]));
    let done = message(7, Author::Bot, "Part and the rest", MessageState::Complete);
    let applied = store.apply(&completed(11, done.clone()));
    assert_eq!(applied.reload, BTreeSet::from([Snapshot::Bots]));
    assert!(store.apply(&completed(12, done)).reload.is_empty());
}

#[test]
fn an_event_for_a_bot_the_list_lacks_reloads_the_list() {
    let mut store = open(10, Vec::new());
    let applied = store.apply(&status(11, 9, BotStatus::Working));
    assert_eq!(applied.reload, BTreeSet::from([Snapshot::Bots]));
}

#[test]
fn reload_all_covers_the_list_and_every_open_conversation() {
    let mut store = open(10, Vec::new());
    let all = store.reload_all();
    assert_eq!(
        all,
        BTreeSet::from([Snapshot::Bots, Snapshot::Messages(BOT)])
    );
    assert!(store.is_loading(Snapshot::Bots) && store.is_loading(Snapshot::Messages(BOT)));
    // Events meanwhile are held for both and replayed onto each.
    let _ = store.apply(&created(31, reply(9, "")));
    let _ = store.apply(&delta(32, 9, 0, "New"));
    let _ = store.load_bots(bots(30, vec![bot(1, "Ada")]));
    let _ = store.load_messages(BOT, page(31, vec![reply(9, "")]));
    assert_eq!(text(&store, 9), "New");
    assert_eq!(
        store.bot(BOT).unwrap().last_message.as_ref().unwrap().text,
        "New"
    );
}

#[test]
fn a_snapshot_that_outlives_the_held_events_loads_again() {
    let mut store = Store::new();
    assert!(store.request(Snapshot::Bots));
    for seq in 1..=(MAX_HELD as u64 + 1) {
        let _ = store.apply(&status(seq, 1, BotStatus::Working));
    }
    let applied = store.load_bots(bots(3, vec![bot(1, "Ada")]));
    assert_eq!(applied.reload, BTreeSet::from([Snapshot::Bots]));
    assert!(store.is_loading(Snapshot::Bots));
    let applied = store.load_bots(bots(MAX_HELD as u64 + 1, vec![bot(1, "Ada")]));
    assert!(applied.reload.is_empty());
    assert!(!store.is_loading(Snapshot::Bots));
}

#[test]
fn a_failed_load_stops_holding_events() {
    let mut store = Store::new();
    assert!(store.request(Snapshot::Bots));
    let _ = store.apply(&status(1, 1, BotStatus::Working));
    store.load_failed(Snapshot::Bots);
    assert!(store.held.is_empty() && !store.is_loading(Snapshot::Bots));
    assert!(store.request(Snapshot::Bots), "it can be asked for again");
}

#[test]
fn the_list_shows_pinned_bots_then_recent_activity_and_hides_hidden_ones() {
    let mut store = Store::new();
    let mut quiet = bot(1, "Quiet");
    quiet.updated_at = Timestamp(5_000);
    let mut busy = bot(2, "Busy");
    busy.last_message = Some(MessagePreview {
        author: Author::User,
        text: "Hi".into(),
        at: Timestamp(9_000),
    });
    let mut pinned = bot(3, "Pinned");
    pinned.pinned = true;
    let mut hidden = bot(4, "Hidden");
    hidden.hidden = true;
    hidden.updated_at = Timestamp(99_000);
    let same_as_quiet = Bot {
        updated_at: Timestamp(5_000),
        ..bot(5, "Also quiet")
    };
    let _ = store.load_bots(bots(1, vec![quiet, busy, pinned, hidden, same_as_quiet]));
    let names: Vec<&str> = store
        .visible_bots()
        .iter()
        .map(|b| b.name.as_str())
        .collect();
    assert_eq!(names, ["Pinned", "Busy", "Quiet", "Also quiet"]);
    assert_eq!(store.bots().count(), 5);
}

#[test]
fn a_closed_conversation_ignores_its_events() {
    let mut store = open(10, vec![reply(7, "")]);
    store.close_conversation(BOT);
    let _ = store.apply(&delta(11, 7, 0, "Hello"));
    assert!(store.conversation(BOT).is_none());
}

#[test]
fn errors_and_usage_are_kept() {
    let mut store = open(10, Vec::new());
    let error = at(
        11,
        Event::BotError {
            bot_id: BOT,
            message: "The Bot could not reply".into(),
        },
    );
    assert!(store.apply(&error).changed);
    assert_eq!(store.error(BOT), Some("The Bot could not reply"));
    let usage = Usage {
        subscription: Vec::new(),
        allowance: Allowance {
            limits: Limits::default(),
            turns_today: 3,
            tokens_this_week: 1_200,
            exhausted: false,
        },
    };
    assert!(
        store
            .apply(&at(
                12,
                Event::UsageUpdated {
                    usage: usage.clone()
                }
            ))
            .changed
    );
    assert_eq!(store.usage(), Some(&usage));
}

#[test]
fn a_usage_reply_older_than_an_event_is_ignored() {
    let mut store = Store::new();
    let usage = |turns_today| Usage {
        subscription: Vec::new(),
        allowance: Allowance {
            limits: Limits::default(),
            turns_today,
            tokens_this_week: 0,
            exhausted: false,
        },
    };
    store.request_usage();
    assert!(store.load_usage(usage(1)));
    assert!(!store.load_usage(usage(1)), "unchanged");
    store.request_usage();
    // An event reports a newer meter before the reply arrives.
    let _ = store.apply(&at(1, Event::UsageUpdated { usage: usage(3) }));
    assert!(!store.load_usage(usage(2)));
    assert_eq!(store.usage(), Some(&usage(3)));
    // A later request applies again.
    store.request_usage();
    assert!(store.load_usage(usage(4)));
    assert_eq!(store.usage(), Some(&usage(4)));
}

#[test]
fn a_late_usage_reply_still_fills_subscription_windows_no_event_has_shown() {
    let mut store = Store::new();
    let usage = |windows: bool| Usage {
        subscription: if windows {
            vec![RateLimitBucket {
                id: "codex".into(),
                name: None,
                primary: None,
                secondary: None,
                reached: true,
            }]
        } else {
            Vec::new()
        },
        allowance: Allowance {
            limits: Limits::default(),
            turns_today: 2,
            tokens_this_week: 0,
            exhausted: false,
        },
    };
    store.request_usage();
    let _ = store.apply(&at(
        1,
        Event::UsageUpdated {
            usage: usage(false),
        },
    ));
    // The reply read the windows afresh; the event had none yet.
    assert!(store.load_usage(usage(true)));
    assert_eq!(store.usage(), Some(&usage(true)));
    // Once the meter holds windows, an older reply changes nothing.
    store.request_usage();
    let _ = store.apply(&at(2, Event::UsageUpdated { usage: usage(true) }));
    assert!(!store.load_usage(usage(false)));
}
