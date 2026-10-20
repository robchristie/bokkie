//! Which of their Bots each user's devices show, as their event sockets
//! say with `ClientFrame::Viewing`. The gateway sends no push about a Bot
//! while one of its user's devices shows it.
//!
//! It is kept in memory only. Each event socket holds a [`Viewer`], which
//! forgets what it said when the socket closes; what it said also lapses
//! [`VIEWING_EXPIRES_MS`] after it last said it, so a device that went
//! away without closing its socket stops counting. A socket can only say
//! it shows one of its own user's Bots: the socket's route checks the Bot
//! before calling [`Viewer::show`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bokkie_protocol::events::VIEWING_EXPIRES_MS;
use bokkie_protocol::{BotId, UserId};

use crate::clock::Clock;

/// What one connection last said it shows, and until when that holds.
#[derive(Debug, Clone, Copy)]
struct Shown {
    bot: BotId,
    until_ms: i64,
}

/// Every user's open connections and the Bot each shows.
#[derive(Debug)]
pub struct Presence {
    clock: Clock,
    next: AtomicU64,
    shown: Mutex<HashMap<UserId, HashMap<u64, Shown>>>,
}

impl Presence {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            next: AtomicU64::new(1),
            shown: Mutex::default(),
        }
    }

    /// A new connection of `user`'s, which shows nothing yet.
    pub fn connect(self: &Arc<Self>, user: UserId) -> Viewer {
        Viewer {
            presence: self.clone(),
            user,
            id: self.next.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Whether one of `user`'s devices shows `bot` now.
    pub fn is_shown(&self, user: UserId, bot: BotId) -> bool {
        let now = self.clock.now_ms();
        self.lock()
            .get(&user)
            .is_some_and(|shown| shown.values().any(|s| s.bot == bot && s.until_ms > now))
    }

    /// The Bots `user`'s devices show now, in id order.
    pub fn shown(&self, user: UserId) -> Vec<BotId> {
        let now = self.clock.now_ms();
        let mut bots: Vec<BotId> = self
            .lock()
            .get(&user)
            .map(|shown| {
                shown
                    .values()
                    .filter(|s| s.until_ms > now)
                    .map(|s| s.bot)
                    .collect()
            })
            .unwrap_or_default();
        bots.sort();
        bots.dedup();
        bots
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<UserId, HashMap<u64, Shown>>> {
        // A panic while holding the lock leaves the map whole: every
        // change is one insert or remove.
        self.shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set(&self, user: UserId, id: u64, bot: Option<BotId>) {
        let mut shown = self.lock();
        match bot {
            Some(bot) => {
                let until_ms = self
                    .clock
                    .now_ms()
                    .saturating_add(VIEWING_EXPIRES_MS as i64);
                shown
                    .entry(user)
                    .or_default()
                    .insert(id, Shown { bot, until_ms });
            }
            None => {
                if let Some(connections) = shown.get_mut(&user) {
                    connections.remove(&id);
                    if connections.is_empty() {
                        shown.remove(&user);
                    }
                }
            }
        }
    }
}

/// One connection's say in [`Presence`], forgotten when dropped.
#[derive(Debug)]
pub struct Viewer {
    presence: Arc<Presence>,
    user: UserId,
    id: u64,
}

impl Viewer {
    /// The connection shows `bot`, which the caller has checked is the
    /// user's, or nothing.
    pub fn show(&self, bot: Option<BotId>) {
        self.presence.set(self.user, self.id, bot);
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.presence.set(self.user, self.id, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence() -> (Arc<Presence>, Clock) {
        let clock = Clock::manual(1_000_000);
        (Arc::new(Presence::new(clock.clone())), clock)
    }

    #[test]
    fn a_shown_bot_counts_until_its_connection_says_otherwise() {
        let (presence, _) = presence();
        let phone = presence.connect(UserId(1));
        assert!(!presence.is_shown(UserId(1), BotId(5)));
        phone.show(Some(BotId(5)));
        assert!(presence.is_shown(UserId(1), BotId(5)));
        assert!(!presence.is_shown(UserId(1), BotId(6)));
        phone.show(Some(BotId(6)));
        assert!(!presence.is_shown(UserId(1), BotId(5)));
        assert!(presence.is_shown(UserId(1), BotId(6)));
        phone.show(None);
        assert!(presence.shown(UserId(1)).is_empty());
    }

    #[test]
    fn each_device_counts_and_closing_one_forgets_only_it() {
        let (presence, _) = presence();
        let phone = presence.connect(UserId(1));
        let laptop = presence.connect(UserId(1));
        phone.show(Some(BotId(5)));
        laptop.show(Some(BotId(5)));
        drop(phone);
        assert!(presence.is_shown(UserId(1), BotId(5)));
        drop(laptop);
        assert!(!presence.is_shown(UserId(1), BotId(5)));
        assert!(presence.lock().is_empty(), "nothing is left behind");
    }

    #[test]
    fn what_a_connection_said_lapses_unless_it_says_it_again() {
        let (presence, clock) = presence();
        let phone = presence.connect(UserId(1));
        phone.show(Some(BotId(5)));
        clock.advance(VIEWING_EXPIRES_MS as i64 - 1);
        assert!(presence.is_shown(UserId(1), BotId(5)));
        phone.show(Some(BotId(5)));
        clock.advance(VIEWING_EXPIRES_MS as i64 - 1);
        assert!(presence.is_shown(UserId(1), BotId(5)));
        clock.advance(1);
        assert!(!presence.is_shown(UserId(1), BotId(5)));
        assert!(presence.shown(UserId(1)).is_empty());
    }

    #[test]
    fn one_users_devices_say_nothing_about_anothers() {
        let (presence, _) = presence();
        let robs = presence.connect(UserId(1));
        robs.show(Some(BotId(5)));
        assert!(!presence.is_shown(UserId(2), BotId(5)));
        assert!(presence.shown(UserId(2)).is_empty());
        let annas = presence.connect(UserId(2));
        annas.show(None);
        drop(annas);
        assert!(presence.is_shown(UserId(1), BotId(5)));
    }
}
