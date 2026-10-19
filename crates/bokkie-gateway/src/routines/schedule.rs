//! When a routine runs: its schedule's occurrences in its owner's time zone.
//!
//! A schedule names local times, which [`jiff`] turns into instants with
//! the system's time-zone database. A local time that does not exist, on a
//! day the clocks go forward, runs at the first valid instant after it: the
//! moment the clocks change. A local time that happens twice, on a day they
//! go back, runs once, the first time. Runs of one routine are at least an
//! hour apart: an occurrence moved forward by a gap that would land within
//! an hour of another occurrence is dropped, since that other one covers
//! it.

use bokkie_protocol::Timestamp;
use bokkie_protocol::api::{Schedule, TimeOfDay, Weekday};
use jiff::civil::{Date, Weekday as CivilWeekday};
use jiff::tz::{AmbiguousOffset, TimeZone};

use crate::clock::HOUR_MS;
use crate::error::{Failure, Result};

/// The longest time-zone name accepted.
const MAX_ZONE_CHARS: usize = 64;

/// The time zone `name` names, if the database has it.
pub fn zone(name: &str) -> Option<TimeZone> {
    if name.is_empty() || name.len() > MAX_ZONE_CHARS {
        return None;
    }
    TimeZone::get(name).ok()
}

/// Checks a time-zone name a user gave and returns the database's own
/// spelling of it, such as `Australia/Adelaide` for `australia/adelaide`.
pub fn check_time_zone(name: &str) -> Result<String> {
    let name = name.trim();
    let zone = zone(name).ok_or_else(|| {
        Failure::invalid("A time zone is an IANA name such as \"Australia/Adelaide\"")
    })?;
    Ok(zone.iana_name().unwrap_or(name).to_owned())
}

/// The zone a stored name names. A name the database no longer has (after
/// a tzdata update) falls back to the default zone, and failing that, UTC.
pub fn stored_zone(name: &str) -> TimeZone {
    zone(name)
        .or_else(|| {
            tracing::warn!(
                zone = name,
                "an unknown stored time zone; using the default"
            );
            zone(bokkie_protocol::api::DEFAULT_TIME_ZONE)
        })
        .unwrap_or(TimeZone::UTC)
}

/// Checks a schedule's shape: at least one day, no day twice, and an
/// interval that divides a day.
pub fn check(schedule: &Schedule) -> Result<()> {
    match schedule {
        Schedule::Daily { .. } | Schedule::Weekdays { .. } => Ok(()),
        Schedule::Days { days, .. } => {
            let mut seen = std::collections::BTreeSet::new();
            if days.is_empty() {
                return Err(Failure::invalid("Choose at least one day"));
            }
            if !days.iter().all(|day| seen.insert(*day)) {
                return Err(Failure::invalid("Each day is chosen once"));
            }
            Ok(())
        }
        Schedule::Hours { every, .. } => {
            if Schedule::EVERY_HOURS.contains(every) {
                Ok(())
            } else {
                Err(Failure::invalid(
                    "A routine repeats every 1, 2, 3, 4, 6, 8 or 12 hours",
                ))
            }
        }
    }
}

fn weekday(day: CivilWeekday) -> Weekday {
    match day {
        CivilWeekday::Monday => Weekday::Mon,
        CivilWeekday::Tuesday => Weekday::Tue,
        CivilWeekday::Wednesday => Weekday::Wed,
        CivilWeekday::Thursday => Weekday::Thu,
        CivilWeekday::Friday => Weekday::Fri,
        CivilWeekday::Saturday => Weekday::Sat,
        CivilWeekday::Sunday => Weekday::Sun,
    }
}

/// Whether the schedule runs on `date`.
fn runs_on(schedule: &Schedule, date: Date) -> bool {
    let day = weekday(date.weekday());
    match schedule {
        Schedule::Daily { .. } | Schedule::Hours { .. } => true,
        Schedule::Weekdays { .. } => !matches!(day, Weekday::Sat | Weekday::Sun),
        Schedule::Days { days, .. } => days.contains(&day),
    }
}

/// The local times of day the schedule names, in minutes after midnight,
/// in order.
fn times(schedule: &Schedule) -> Vec<u32> {
    let at = |t: &TimeOfDay| u32::from(t.hour()) * 60 + u32::from(t.minute());
    match schedule {
        Schedule::Daily { at: t } | Schedule::Weekdays { at: t } | Schedule::Days { at: t, .. } => {
            vec![at(t)]
        }
        Schedule::Hours { every, at: t } => {
            let step = u32::from((*every).max(1)) * 60;
            let first = at(t) % step;
            (0..)
                .map(|k| first + k * step)
                .take_while(|m| *m < 24 * 60)
                .collect()
        }
    }
}

/// One candidate instant, and whether a gap moved it forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    at: i64,
    moved: bool,
}

/// The instant a local time on `date` names in `zone`: the first instant
/// at or after it for a time in a gap, the first of two for a time in a
/// fold.
fn resolve(zone: &TimeZone, date: Date, minutes: u32) -> Option<Candidate> {
    let dt = date.at((minutes / 60) as i8, (minutes % 60) as i8, 0, 0);
    let ambiguous = zone.to_ambiguous_timestamp(dt);
    let millis = |offset: jiff::tz::Offset| -> Option<i64> {
        offset.to_timestamp(dt).ok().map(|t| t.as_millisecond())
    };
    match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { offset } => Some(Candidate {
            at: millis(offset)?,
            moved: false,
        }),
        // The earlier instant is the one with the offset before the
        // transition.
        AmbiguousOffset::Fold { before, .. } => Some(Candidate {
            at: millis(before)?,
            moved: false,
        }),
        // Read with the offset after the transition, the local time is an
        // instant before it; the transition itself is the first valid
        // instant after the local time.
        AmbiguousOffset::Gap { after, .. } => {
            let before_gap = after.to_timestamp(dt).ok()?;
            let transition = zone.following(before_gap).next()?;
            Some(Candidate {
                at: transition.timestamp().as_millisecond(),
                moved: true,
            })
        }
    }
}

/// The schedule's occurrences on the local dates `from` to `to`, in order,
/// at least an hour apart.
fn occurrences(schedule: &Schedule, zone: &TimeZone, from: Date, to: Date) -> Vec<i64> {
    let times = times(schedule);
    let mut candidates = Vec::new();
    let mut date = from;
    while date <= to {
        if runs_on(schedule, date) {
            candidates.extend(times.iter().filter_map(|m| resolve(zone, date, *m)));
        }
        match date.tomorrow() {
            Ok(next) => date = next,
            Err(_) => break,
        }
    }
    candidates.sort_by_key(|c| (c.at, c.moved));
    // A moved time can land on a time the schedule names anyway.
    candidates.dedup_by_key(|c| c.at);
    // A moved time within an hour of another occurrence is covered by it.
    let kept: Vec<i64> = candidates
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            !c.moved
                || candidates
                    .iter()
                    .enumerate()
                    .all(|(j, other)| *i == j || (other.at - c.at).abs() >= HOUR_MS)
        })
        .map(|(_, c)| c.at)
        .collect();
    kept
}

fn local_date(zone: &TimeZone, at: i64) -> Option<Date> {
    let timestamp = jiff::Timestamp::from_millisecond(at).ok()?;
    Some(timestamp.to_zoned(zone.clone()).date())
}

/// Dates `days` before and after `date`, clamped to jiff's range.
fn around(date: Date, before: i64, after: i64) -> (Date, Date) {
    let shift = |days: i64| {
        date.checked_add(jiff::Span::new().days(days))
            .unwrap_or(date)
    };
    (shift(-before), shift(after))
}

/// The schedule's first occurrence strictly after `after`.
pub fn next_after(schedule: &Schedule, zone: &TimeZone, after: Timestamp) -> Option<Timestamp> {
    let today = local_date(zone, after.0)?;
    // A week and a day covers every schedule; a day before covers a date
    // the instant's local day starts before.
    let (from, to) = around(today, 1, 8);
    occurrences(schedule, zone, from, to)
        .into_iter()
        .find(|at| *at > after.0)
        .map(Timestamp)
}

/// The schedule's last occurrence at or before `now`.
pub fn latest_at_or_before(
    schedule: &Schedule,
    zone: &TimeZone,
    now: Timestamp,
) -> Option<Timestamp> {
    let today = local_date(zone, now.0)?;
    let (from, to) = around(today, 8, 1);
    occurrences(schedule, zone, from, to)
        .into_iter()
        .rev()
        .find(|at| *at <= now.0)
        .map(Timestamp)
}

/// A scheduled time as its owner reads it, such as `7:00`.
pub fn local_time(zone: &TimeZone, at: Timestamp) -> String {
    match jiff::Timestamp::from_millisecond(at.0) {
        Ok(timestamp) => {
            let local = timestamp.to_zoned(zone.clone());
            format!("{}:{:02}", local.hour(), local.minute())
        }
        Err(_) => String::new(),
    }
}

/// A scheduled time in full, for the model, such as
/// `07:00 on Monday 20 October 2026 (Australia/Adelaide)`.
pub fn local_description(zone: &TimeZone, at: Timestamp) -> String {
    match jiff::Timestamp::from_millisecond(at.0) {
        Ok(timestamp) => {
            let local = timestamp.to_zoned(zone.clone());
            format!(
                "{} ({})",
                local.strftime("%H:%M on %A %-d %B %Y"),
                zone.iana_name().unwrap_or("UTC")
            )
        }
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adelaide() -> TimeZone {
        zone("Australia/Adelaide").expect("the system's tzdata has Australia/Adelaide")
    }

    fn at(text: &str) -> TimeOfDay {
        text.parse().unwrap()
    }

    /// The instant of a local time in Adelaide, written with its offset,
    /// such as `2026-10-04T03:00+10:30`.
    fn instant(text: &str) -> Timestamp {
        Timestamp(
            text.parse::<jiff::Timestamp>()
                .unwrap_or_else(|e| panic!("{text}: {e}"))
                .as_millisecond(),
        )
    }

    fn local(zone: &TimeZone, at: Timestamp) -> String {
        jiff::Timestamp::from_millisecond(at.0)
            .unwrap()
            .to_zoned(zone.clone())
            .strftime("%Y-%m-%dT%H:%M%:z")
            .to_string()
    }

    /// The next `count` occurrences after `start`.
    fn next(schedule: &Schedule, zone: &TimeZone, start: &str, count: usize) -> Vec<String> {
        let mut after = instant(start);
        let mut seen = Vec::new();
        for _ in 0..count {
            after = next_after(schedule, zone, after).unwrap();
            seen.push(local(zone, after));
        }
        seen
    }

    #[test]
    fn time_zone_names_are_checked_and_spelt_as_the_database_does() {
        assert_eq!(
            check_time_zone("Australia/Adelaide").unwrap(),
            "Australia/Adelaide"
        );
        assert_eq!(
            check_time_zone(" australia/adelaide ").unwrap(),
            "Australia/Adelaide"
        );
        assert_eq!(check_time_zone("UTC").unwrap(), "UTC");
        for bad in [
            "",
            "Mars/Olympus_Mons",
            "+09:30",
            "../../etc/passwd",
            &"A".repeat(65),
        ] {
            assert!(check_time_zone(bad).is_err(), "{bad}");
        }
        assert_eq!(
            stored_zone("Gone/Away").iana_name(),
            Some("Australia/Adelaide")
        );
    }

    #[test]
    fn schedules_are_checked() {
        let seven = at("07:00");
        assert!(check(&Schedule::Daily { at: seven }).is_ok());
        assert!(
            check(&Schedule::Days {
                days: vec![],
                at: seven
            })
            .is_err()
        );
        assert!(
            check(&Schedule::Days {
                days: vec![Weekday::Mon, Weekday::Mon],
                at: seven
            })
            .is_err()
        );
        for every in [1, 2, 3, 4, 6, 8, 12] {
            assert!(check(&Schedule::Hours { every, at: seven }).is_ok());
        }
        for every in [0, 5, 7, 24, 48] {
            assert!(check(&Schedule::Hours { every, at: seven }).is_err());
        }
    }

    #[test]
    fn daily_weekday_and_chosen_day_schedules_run_at_their_local_time() {
        let zone = adelaide();
        // Monday 19 October 2026, after the October change: +10:30.
        let start = "2026-10-19T06:00+10:30";
        assert_eq!(
            next(&Schedule::Daily { at: at("07:00") }, &zone, start, 3),
            [
                "2026-10-19T07:00+10:30",
                "2026-10-20T07:00+10:30",
                "2026-10-21T07:00+10:30"
            ]
        );
        // Exactly at the time is not after it.
        assert_eq!(
            next(
                &Schedule::Daily { at: at("07:00") },
                &zone,
                "2026-10-19T07:00+10:30",
                1
            ),
            ["2026-10-20T07:00+10:30"]
        );
        assert_eq!(
            next(
                &Schedule::Weekdays { at: at("07:00") },
                &zone,
                "2026-10-23T08:00+10:30",
                2
            ),
            ["2026-10-26T07:00+10:30", "2026-10-27T07:00+10:30"],
            "Friday's run has passed; Monday is next"
        );
        let days = Schedule::Days {
            days: vec![Weekday::Sun, Weekday::Wed],
            at: at("21:30"),
        };
        assert_eq!(
            next(&days, &zone, start, 3),
            [
                "2026-10-21T21:30+10:30",
                "2026-10-25T21:30+10:30",
                "2026-10-28T21:30+10:30"
            ]
        );
        // One chosen day a week: next week's.
        let saturdays = Schedule::Days {
            days: vec![Weekday::Sat],
            at: at("06:00"),
        };
        assert_eq!(
            next(&saturdays, &zone, "2026-10-24T06:00+10:30", 1),
            ["2026-10-31T06:00+10:30"]
        );
    }

    #[test]
    fn hourly_schedules_run_through_the_day_from_their_time() {
        let zone = adelaide();
        let every_six = Schedule::Hours {
            every: 6,
            at: at("07:15"),
        };
        assert_eq!(
            next(&every_six, &zone, "2026-10-19T06:00+10:30", 5),
            [
                "2026-10-19T07:15+10:30",
                "2026-10-19T13:15+10:30",
                "2026-10-19T19:15+10:30",
                "2026-10-20T01:15+10:30",
                "2026-10-20T07:15+10:30"
            ]
        );
    }

    /// Adelaide's clocks go forward from 02:00 +09:30 to 03:00 +10:30 on
    /// Sunday 4 October 2026.
    #[test]
    fn a_time_the_clocks_skip_in_october_runs_when_they_change() {
        let zone = adelaide();
        let daily = Schedule::Daily { at: at("02:30") };
        assert_eq!(
            next(&daily, &zone, "2026-10-03T12:00+09:30", 3),
            [
                // 02:30 does not exist: it runs at the change.
                "2026-10-04T03:00+10:30",
                "2026-10-05T02:30+10:30",
                "2026-10-06T02:30+10:30"
            ]
        );
        // The start of the gap runs at the change too.
        assert_eq!(
            next(
                &Schedule::Daily { at: at("02:00") },
                &zone,
                "2026-10-03T12:00+09:30",
                1
            ),
            ["2026-10-04T03:00+10:30"]
        );
        // Times either side are unaffected.
        assert_eq!(
            next(
                &Schedule::Daily { at: at("01:59") },
                &zone,
                "2026-10-03T12:00+09:30",
                1
            ),
            ["2026-10-04T01:59+09:30"]
        );
        assert_eq!(
            next(
                &Schedule::Daily { at: at("03:00") },
                &zone,
                "2026-10-03T12:00+09:30",
                1
            ),
            ["2026-10-04T03:00+10:30"]
        );
        // Hourly runs stay at least an hour apart: the skipped 02:30 is
        // covered by 03:30, half an hour after the change.
        let hourly = Schedule::Hours {
            every: 1,
            at: at("00:30"),
        };
        let runs = next(&hourly, &zone, "2026-10-04T00:00+09:30", 4);
        assert_eq!(
            runs,
            [
                "2026-10-04T00:30+09:30",
                "2026-10-04T01:30+09:30",
                "2026-10-04T03:30+10:30",
                "2026-10-04T04:30+10:30"
            ]
        );
        // On the hour, the skipped 02:00 is 03:00 itself, once.
        let on_the_hour = Schedule::Hours {
            every: 1,
            at: at("00:00"),
        };
        assert_eq!(
            next(&on_the_hour, &zone, "2026-10-04T01:30+09:30", 2),
            ["2026-10-04T03:00+10:30", "2026-10-04T04:00+10:30"]
        );
        // Two-hourly at half past: the change, an hour and a half after
        // 00:30 and before 04:30, is far enough from both.
        let two_hourly = Schedule::Hours {
            every: 2,
            at: at("00:30"),
        };
        assert_eq!(
            next(&two_hourly, &zone, "2026-10-04T00:00+09:30", 3),
            [
                "2026-10-04T00:30+09:30",
                "2026-10-04T03:00+10:30",
                "2026-10-04T04:30+10:30"
            ]
        );
    }

    /// Adelaide's clocks go back from 03:00 +10:30 to 02:00 +09:30 on
    /// Sunday 5 April 2026.
    #[test]
    fn a_time_that_happens_twice_in_april_runs_once() {
        let zone = adelaide();
        let daily = Schedule::Daily { at: at("02:30") };
        assert_eq!(
            next(&daily, &zone, "2026-04-04T12:00+10:30", 3),
            [
                "2026-04-05T02:30+10:30",
                "2026-04-06T02:30+09:30",
                "2026-04-07T02:30+09:30"
            ]
        );
        let hourly = Schedule::Hours {
            every: 1,
            at: at("00:30"),
        };
        let runs = next(&hourly, &zone, "2026-04-05T00:00+10:30", 4);
        assert_eq!(
            runs,
            [
                "2026-04-05T00:30+10:30",
                "2026-04-05T01:30+10:30",
                "2026-04-05T02:30+10:30",
                "2026-04-05T03:30+09:30"
            ],
            "02:30 runs once, the first time; 03:30 is two hours later"
        );
    }

    #[test]
    fn runs_of_a_routine_are_always_at_least_an_hour_apart() {
        let zone = adelaide();
        for every in Schedule::EVERY_HOURS {
            for minute in [0, 1, 29, 30, 59] {
                for hour in 0..24 {
                    let schedule = Schedule::Hours {
                        every,
                        at: TimeOfDay::new(hour, minute).unwrap(),
                    };
                    for start in ["2026-04-04T00:00+10:30", "2026-10-03T00:00+09:30"] {
                        let mut previous = instant(start);
                        for _ in 0..(72 / usize::from(every)) {
                            let at = next_after(&schedule, &zone, previous).unwrap();
                            assert!(
                                at.0 - previous.0 >= HOUR_MS || previous == instant(start),
                                "{schedule:?} from {start}: {} then {}",
                                local(&zone, previous),
                                local(&zone, at)
                            );
                            previous = at;
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_latest_occurrence_is_found_for_misfires() {
        let zone = adelaide();
        let daily = Schedule::Daily { at: at("07:00") };
        assert_eq!(
            latest_at_or_before(&daily, &zone, instant("2026-10-21T06:59+10:30")),
            Some(instant("2026-10-20T07:00+10:30"))
        );
        assert_eq!(
            latest_at_or_before(&daily, &zone, instant("2026-10-21T07:00+10:30")),
            Some(instant("2026-10-21T07:00+10:30"))
        );
        let saturdays = Schedule::Days {
            days: vec![Weekday::Sat],
            at: at("06:00"),
        };
        assert_eq!(
            latest_at_or_before(&saturdays, &zone, instant("2026-10-30T05:00+10:30")),
            Some(instant("2026-10-24T06:00+10:30"))
        );
    }

    #[test]
    fn scheduled_times_read_as_their_owner_reads_them() {
        let zone = adelaide();
        assert_eq!(local_time(&zone, instant("2026-10-19T07:00+10:30")), "7:00");
        assert_eq!(
            local_time(&zone, instant("2026-10-19T21:05+10:30")),
            "21:05"
        );
        assert_eq!(
            local_description(&zone, instant("2026-10-19T07:00+10:30")),
            "07:00 on Monday 19 October 2026 (Australia/Adelaide)"
        );
    }
}
