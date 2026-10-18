//! The usage meter and the limit-reached banner, as text.
//!
//! Reset times are said relative to now ("in 2 h 10 min"), so they need no
//! time zone; the page shows them again each minute while they are on
//! screen.

use bokkie_protocol::Timestamp;
use bokkie_protocol::api::{RateLimitBucket, RateLimitWindow, Usage};

const MINUTE: i64 = 60_000;

/// How long until `at`, as "in 2 h 10 min".
pub fn resets_in(now: Timestamp, at: Timestamp) -> String {
    let left = at.0 - now.0;
    if left < MINUTE {
        return "in under a minute".into();
    }
    // Round up, so a reset never shows as sooner than it is.
    let minutes = (left + MINUTE - 1) / MINUTE;
    let (days, hours, minutes) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
    match (days, hours, minutes) {
        (0, 0, m) => format!("in {m} min"),
        (0, h, 0) => format!("in {h} h"),
        (0, h, m) => format!("in {h} h {m} min"),
        (1, 0, _) => "in 1 day".into(),
        (1, h, _) => format!("in 1 day {h} h"),
        (d, _, _) => format!("in {d} days"),
    }
}

/// A rate-limit window's length, as "5-hour" or "weekly".
fn window_name(minutes: Option<i64>) -> String {
    match minutes {
        Some(10_080) => "weekly".into(),
        Some(1_440) => "daily".into(),
        Some(m) if m > 0 && m % 60 == 0 => format!("{}-hour", m / 60),
        Some(m) if m > 0 => format!("{m}-minute"),
        _ => "current".into(),
    }
}

fn window_line(bucket: &str, window: &RateLimitWindow, now: Timestamp) -> String {
    let mut line = format!(
        "{bucket}: {}% of the {} limit used",
        window.used_percent.clamp(0, 100),
        window_name(window.window_minutes)
    );
    if let Some(at) = window.resets_at {
        line.push_str(&format!(", resets {}", resets_in(now, at)));
    }
    line
}

fn bucket_name(bucket: &RateLimitBucket) -> &str {
    bucket.name.as_deref().unwrap_or(&bucket.id)
}

/// The meter's lines: each subscription window, then the user's allowance.
pub fn meter_lines(usage: &Usage, now: Timestamp) -> Vec<String> {
    let mut lines = Vec::new();
    for bucket in &usage.subscription {
        let name = bucket_name(bucket);
        let windows: Vec<&RateLimitWindow> = [&bucket.primary, &bucket.secondary]
            .into_iter()
            .flatten()
            .collect();
        if windows.is_empty() && bucket.reached {
            lines.push(format!("{name}: limit reached"));
        }
        for window in windows {
            lines.push(window_line(name, window, now));
        }
    }
    if usage.subscription.is_empty() {
        lines.push("Subscription: not reported yet".into());
    }
    let allowance = &usage.allowance;
    lines.push(match allowance.limits.daily_turns {
        Some(limit) => format!("Your turns today: {} of {limit}", allowance.turns_today),
        None => format!("Your turns today: {}, no limit", allowance.turns_today),
    });
    lines.push(match allowance.limits.weekly_tokens {
        Some(limit) => format!(
            "Your tokens this week: {} of {}",
            thousands(allowance.tokens_this_week),
            thousands(limit)
        ),
        None => format!(
            "Your tokens this week: {}, no limit",
            thousands(allowance.tokens_this_week)
        ),
    });
    lines
}

/// The limit-reached banner, when a subscription bucket is reached or the
/// user's allowance is used up.
pub fn limit_banner(usage: &Usage, now: Timestamp) -> Option<String> {
    if let Some(bucket) = usage.subscription.iter().find(|bucket| bucket.reached) {
        let windows = || [&bucket.primary, &bucket.secondary].into_iter().flatten();
        // The full windows decide when replies can resume; failing those,
        // any window that reports a reset.
        let full = windows()
            .filter(|window| window.used_percent >= 100)
            .filter_map(|window| window.resets_at)
            .max();
        let reset = full.or_else(|| windows().filter_map(|window| window.resets_at).max());
        let when = reset.map_or("once it resets".into(), |at| resets_in(now, at));
        return Some(format!(
            "The shared ChatGPT subscription has reached its limit. Bots can reply again {when}"
        ));
    }
    usage.allowance.exhausted.then(|| {
        "You have used your allowance for now. It refills over the next day or week".into()
    })
}

/// 12345 as "12,345".
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use bokkie_protocol::api::{Allowance, Limits};

    use super::*;

    const NOW: Timestamp = Timestamp(1_800_000_000_000);
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    fn after(millis: i64) -> Timestamp {
        Timestamp(NOW.0 + millis)
    }

    fn window(used_percent: i32, minutes: i64, resets_in: i64) -> RateLimitWindow {
        RateLimitWindow {
            used_percent,
            window_minutes: Some(minutes),
            resets_at: Some(after(resets_in)),
        }
    }

    fn usage(subscription: Vec<RateLimitBucket>, allowance: Allowance) -> Usage {
        Usage {
            subscription,
            allowance,
        }
    }

    fn allowance(exhausted: bool) -> Allowance {
        Allowance {
            limits: Limits {
                daily_turns: Some(20),
                weekly_tokens: None,
            },
            turns_today: 3,
            tokens_this_week: 1_234_567,
            exhausted,
        }
    }

    #[test]
    fn reset_times_are_said_relative_to_now() {
        assert_eq!(resets_in(NOW, after(30_000)), "in under a minute");
        assert_eq!(resets_in(NOW, after(-5_000)), "in under a minute");
        assert_eq!(resets_in(NOW, after(61_000)), "in 2 min");
        assert_eq!(resets_in(NOW, after(2 * HOUR)), "in 2 h");
        assert_eq!(
            resets_in(NOW, after(2 * HOUR + 10 * MINUTE)),
            "in 2 h 10 min"
        );
        assert_eq!(resets_in(NOW, after(DAY + 3 * HOUR)), "in 1 day 3 h");
        assert_eq!(resets_in(NOW, after(DAY)), "in 1 day");
        assert_eq!(resets_in(NOW, after(5 * DAY)), "in 5 days");
    }

    #[test]
    fn the_meter_lists_each_window_and_the_allowance() {
        let codex = RateLimitBucket {
            id: "codex".into(),
            name: Some("Codex".into()),
            primary: Some(window(42, 300, 2 * HOUR)),
            secondary: Some(window(7, 10_080, 3 * DAY)),
            reached: false,
        };
        let other = RateLimitBucket {
            id: "other".into(),
            name: None,
            primary: None,
            secondary: None,
            reached: true,
        };
        assert_eq!(
            meter_lines(&usage(vec![codex, other], allowance(false)), NOW),
            [
                "Codex: 42% of the 5-hour limit used, resets in 2 h",
                "Codex: 7% of the weekly limit used, resets in 3 days",
                "other: limit reached",
                "Your turns today: 3 of 20",
                "Your tokens this week: 1,234,567, no limit",
            ]
        );
        assert_eq!(
            meter_lines(&usage(Vec::new(), allowance(false)), NOW)[0],
            "Subscription: not reported yet"
        );
    }

    #[test]
    fn the_banner_says_when_a_reached_subscription_resets() {
        let bucket = |reached, primary, secondary| RateLimitBucket {
            id: "codex".into(),
            name: None,
            primary,
            secondary,
            reached,
        };
        assert_eq!(
            limit_banner(
                &usage(
                    vec![bucket(false, Some(window(99, 300, HOUR)), None)],
                    allowance(false)
                ),
                NOW
            ),
            None
        );
        assert_eq!(
            limit_banner(
                &usage(
                    vec![bucket(
                        true,
                        Some(window(100, 300, 2 * HOUR)),
                        Some(window(60, 10_080, 3 * DAY))
                    )],
                    allowance(false)
                ),
                NOW
            )
            .as_deref(),
            Some(
                "The shared ChatGPT subscription has reached its limit. Bots can reply again in 2 h"
            )
        );
        assert_eq!(
            limit_banner(
                &usage(vec![bucket(true, None, None)], allowance(false)),
                NOW
            )
            .as_deref(),
            Some(
                "The shared ChatGPT subscription has reached its limit. Bots can reply again once it resets"
            )
        );
    }

    #[test]
    fn the_banner_says_when_the_allowance_is_used_up() {
        assert_eq!(
            limit_banner(&usage(Vec::new(), allowance(true)), NOW).as_deref(),
            Some("You have used your allowance for now. It refills over the next day or week")
        );
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
    }
}
