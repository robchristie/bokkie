use std::str::FromStr;

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecurrenceError {
    #[error("unknown IANA time zone {0:?}")]
    TimeZone(String),
    #[error("invalid cron expression: {0}")]
    Cron(String),
    #[error("timestamp is outside the supported range")]
    Timestamp,
    #[error("cron expression has no later occurrence")]
    Exhausted,
}

/// A cron schedule evaluated in a named IANA time zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    expression: String,
    timezone: String,
}

impl Recurrence {
    pub fn new(
        expression: impl Into<String>,
        timezone: impl Into<String>,
    ) -> Result<Self, RecurrenceError> {
        let recurrence = Self {
            expression: expression.into(),
            timezone: timezone.into(),
        };
        recurrence.parsed()?;
        Ok(recurrence)
    }

    pub fn expression(&self) -> &str {
        &self.expression
    }

    pub fn timezone(&self) -> &str {
        &self.timezone
    }

    /// Return the first scheduled instant strictly after `after`, as UTC Unix seconds.
    pub fn next_after(&self, after: i64) -> Result<i64, RecurrenceError> {
        let (schedule, timezone) = self.parsed()?;
        let after = Utc
            .timestamp_opt(after, 0)
            .single()
            .ok_or(RecurrenceError::Timestamp)?
            .with_timezone(&timezone);
        schedule
            .after(&after)
            .next()
            .map(|value| value.with_timezone(&Utc).timestamp())
            .ok_or(RecurrenceError::Exhausted)
    }

    fn parsed(&self) -> Result<(Schedule, Tz), RecurrenceError> {
        let timezone = Tz::from_str(&self.timezone)
            .map_err(|_| RecurrenceError::TimeZone(self.timezone.clone()))?;
        let cron = normalise_cron(&self.expression)?;
        let schedule =
            Schedule::from_str(&cron).map_err(|error| RecurrenceError::Cron(error.to_string()))?;
        Ok((schedule, timezone))
    }
}

fn normalise_cron(expression: &str) -> Result<String, RecurrenceError> {
    let fields = expression.split_whitespace().count();
    match fields {
        // The cron crate includes seconds; accept the familiar five-field form too.
        5 => Ok(format!("0 {expression}")),
        6 | 7 => Ok(expression.to_owned()),
        _ => Err(RecurrenceError::Cron(format!(
            "expected 5, 6, or 7 fields, found {fields}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_zone_schedule_tracks_daylight_saving() {
        let recurrence = Recurrence::new("30 9 * * *", "Australia/Adelaide").unwrap();
        let winter_before = Utc
            .with_ymd_and_hms(2026, 6, 30, 23, 59, 59)
            .unwrap()
            .timestamp();
        let winter_occurrence = Utc
            .with_ymd_and_hms(2026, 7, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        let summer_before = Utc
            .with_ymd_and_hms(2026, 1, 1, 22, 59, 59)
            .unwrap()
            .timestamp();
        let summer_occurrence = Utc
            .with_ymd_and_hms(2026, 1, 1, 23, 0, 0)
            .unwrap()
            .timestamp();

        assert_eq!(
            recurrence.next_after(winter_before).unwrap(),
            winter_occurrence
        );
        assert_eq!(
            recurrence.next_after(summer_before).unwrap(),
            summer_occurrence
        );
    }
}
