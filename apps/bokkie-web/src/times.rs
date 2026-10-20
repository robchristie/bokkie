//! Times for people, as local times in the user's own zone: when a message
//! was written, when oversight was switched on, when an admin reset a
//! password. Countdowns, such as when a limit resets, stay relative
//! ([`crate::usage`]).

use std::time::Duration;

use bokkie_protocol::Timestamp;
use stobie_ui_egui::local_time::{self, LocalTime};

/// How long a screen that shows local times waits at most before it is
/// shown again, so a daylight-saving change is not missed for long.
const LONGEST_WAIT: Duration = Duration::from_secs(60 * 60);

/// A gateway time as Stobie's instant, held to the range Stobie supports.
pub(crate) fn instant(at: Timestamp) -> local_time::Timestamp {
    let millis = at.0.clamp(
        local_time::Timestamp::MIN.unix_millis(),
        local_time::Timestamp::MAX.unix_millis(),
    );
    local_time::Timestamp::from_unix_millis(millis).unwrap_or(local_time::Timestamp::MIN)
}

/// When a message was written: "9:41 AM" today, otherwise its day too,
/// as "Yesterday at 9:41 AM" or "14 Apr at 9:41 AM".
pub(crate) fn message_time(local: &LocalTime, at: Timestamp) -> String {
    let at = instant(at);
    let day = |civil: local_time::CivilDateTime| (civil.year, civil.month, civil.day);
    if day(local.civil(at)) == day(local.civil(local.now())) {
        local.time_of_day(at)
    } else {
        local.relative_day_and_time(at)
    }
}

/// When a Bot's latest message arrived, for its row in the list: "9:41 AM"
/// today, otherwise its day, such as "Yesterday" or "14 Apr".
pub(crate) fn compact(local: &LocalTime, at: Timestamp) -> String {
    local.compact(instant(at))
}

/// When something happened, inside a sentence: "today at 9:41 AM",
/// "yesterday at 9:41 AM", "on Monday at 9:41 AM" or "on 14 Apr at
/// 9:41 AM".
pub(crate) fn on_day_at(local: &LocalTime, at: Timestamp) -> String {
    let day = day_at(local, at);
    // "today", "yesterday" and "tomorrow" need no "on".
    if day.starts_with(|c: char| c.is_lowercase()) {
        day
    } else {
        format!("on {day}")
    }
}

/// When something happened, inside a sentence after a word such as
/// "since": "today at 9:41 AM", "Monday at 9:41 AM" or "14 Apr at 9:41 AM".
pub(crate) fn day_at(local: &LocalTime, at: Timestamp) -> String {
    let text = local.relative_day_and_time(instant(at));
    // Stobie's relative days begin a sentence; these sit inside one.
    for word in ["Today", "Yesterday", "Tomorrow"] {
        if let Some(rest) = text.strip_prefix(word) {
            return format!("{}{rest}", word.to_lowercase());
        }
    }
    text
}

/// How long until the day changes on the user's clock, so that "9:41 AM"
/// becomes "Yesterday at 9:41 AM" at midnight, waiting an hour at most.
pub(crate) fn until_the_day_changes(local: &LocalTime) -> Duration {
    let now = local.civil(local.now());
    let into_day = u64::from(now.hour) * 3_600 + u64::from(now.minute) * 60 + u64::from(now.second);
    let left = Duration::from_secs(86_400u64.saturating_sub(into_day));
    // A second past midnight, so the day has changed when the page shows.
    (left + Duration::from_secs(1)).min(LONGEST_WAIT)
}

#[cfg(test)]
mod tests {
    use stobie_ui_egui::local_time::{Clock, TimeLocale, TimeZone};

    use super::*;

    /// 2026-04-14T00:11:00Z: Tuesday 14 April, 9:41 AM in Adelaide.
    const NOW: i64 = 1_776_125_460_000;
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;

    fn adelaide() -> LocalTime {
        LocalTime::new(
            TimeZone::named("Australia/Adelaide").unwrap(),
            TimeLocale::for_language_tag("en-AU"),
            Clock::Fixed(instant(Timestamp(NOW))),
        )
    }

    #[test]
    fn a_message_shows_its_time_and_its_day_when_not_today() {
        let local = adelaide();
        assert_eq!(message_time(&local, Timestamp(NOW)), "9:41 AM");
        assert_eq!(message_time(&local, Timestamp(NOW - 9 * HOUR)), "12:41 AM");
        assert_eq!(
            message_time(&local, Timestamp(NOW - 10 * HOUR)),
            "Yesterday at 11:41 PM"
        );
        assert_eq!(
            message_time(&local, Timestamp(NOW - 3 * DAY)),
            "Saturday at 9:41 AM"
        );
        assert_eq!(
            message_time(&local, Timestamp(NOW - 30 * DAY)),
            "15 Mar at 10:41 AM",
            "the hour moves back with daylight saving"
        );
    }

    #[test]
    fn a_time_inside_a_sentence_reads_as_one() {
        let local = adelaide();
        assert_eq!(on_day_at(&local, Timestamp(NOW)), "today at 9:41 AM");
        assert_eq!(
            on_day_at(&local, Timestamp(NOW - DAY)),
            "yesterday at 9:41 AM"
        );
        assert_eq!(
            on_day_at(&local, Timestamp(NOW - 3 * DAY)),
            "on Saturday at 9:41 AM"
        );
        assert_eq!(
            on_day_at(&local, Timestamp(NOW - 30 * DAY)),
            "on 15 Mar at 10:41 AM"
        );
        assert_eq!(
            day_at(&local, Timestamp(NOW - 3 * DAY)),
            "Saturday at 9:41 AM"
        );
        assert_eq!(day_at(&local, Timestamp(NOW + DAY)), "tomorrow at 9:41 AM");
    }

    #[test]
    fn the_page_is_shown_again_when_the_day_changes() {
        let local = adelaide();
        assert_eq!(until_the_day_changes(&local), LONGEST_WAIT);
        // 11:41 PM: the day changes in 19 minutes.
        let late = local.with_clock(Clock::Fixed(instant(Timestamp(NOW + 14 * HOUR))));
        assert_eq!(
            until_the_day_changes(&late),
            Duration::from_secs(19 * 60 + 1)
        );
    }

    #[test]
    fn times_outside_stobies_range_are_held_to_it() {
        assert_eq!(instant(Timestamp(i64::MIN)), local_time::Timestamp::MIN);
        assert_eq!(instant(Timestamp(i64::MAX)), local_time::Timestamp::MAX);
    }
}
