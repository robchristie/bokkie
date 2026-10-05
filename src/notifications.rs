//! Durable notification payloads and the external sender boundary.
use std::{io::Read, path::Path};

use serde::{Deserialize, Serialize};

mod smtp;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationIntent {
    pub id: String,
    pub task_id: String,
    pub source_obligation_id: String,
    pub destination: String,
    pub subject: String,
    pub body: String,
    pub message_id: String,
    pub created_at: i64,
    pub transport: Option<NotificationTransport>,
}

/// Immutable first-attempt transport identity. Changing runtime settings cannot
/// silently change the sender, relay or recipient of an accepted intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NotificationTransport {
    pub relay_host: String,
    pub relay_port: u16,
    pub from_address: String,
    pub destination: String,
}

/// Only proof that the relay did not accept the message permits safe retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationOutcome {
    Accepted { detail: String },
    Rejected { retryable: bool, detail: String },
    Uncertain { detail: String },
}

pub trait NotificationSender {
    /// Runs after Store commits the intent and possible-dispatch marker.
    fn send(&self, intent: &NotificationIntent) -> NotificationOutcome;
    fn transport(&self) -> Option<NotificationTransport> {
        None
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationConfig {
    pub relay_host: String,
    pub relay_port: u16,
    pub from_address: String,
    pub destination: String,
    pub timeout_ms: u64,
}
impl NotificationConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err(
                "notification configuration must be an absolute file path within 8192 bytes".into(),
            );
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("notification configuration must be a regular JSON file".into());
        }
        let mut raw = Vec::new();
        file.take(8193)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > 8192 {
            return Err("notification configuration exceeds 8192 bytes".into());
        }
        let value: Self = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        value.validate()?;
        Ok(value)
    }
    pub fn destination(&self) -> &str {
        &self.destination
    }
    pub fn validate(&self) -> Result<(), String> {
        validate_address(&self.destination)?;
        validate_address(&self.from_address)?;
        if self.relay_port == 0
            || self.relay_host.is_empty()
            || self.relay_host.len() > 253
            || self
                .relay_host
                .bytes()
                .any(|c| !c.is_ascii_alphanumeric() && !b".-:".contains(&c))
            || !(100..=3000).contains(&self.timeout_ms)
        {
            return Err(
                "notification relay requires a host, port and total timeout within 100..=3000 ms"
                    .into(),
            );
        }
        Ok(())
    }
}

pub fn validate_address(address: &str) -> Result<(), String> {
    let parts: Vec<_> = address.split('@').collect();
    if address.len() > 200
        || parts.len() != 2
        || parts.iter().any(|p| p.is_empty())
        || parts[0].len() > 64
        || parts[0].starts_with('.')
        || parts[0].ends_with('.')
        || parts[0].contains("..")
        || !parts[1].contains('.')
        || parts[1].split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || label
                    .bytes()
                    .any(|c| !c.is_ascii_alphanumeric() && c != b'-')
        })
        || address
            .bytes()
            .any(|c| !c.is_ascii_alphanumeric() && !b".!#$%&'*+-/=?^_`{|}~@".contains(&c))
    {
        return Err(
            "notification destination must be one bare ASCII email address within 200 characters"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_is_bounded_strict_read_only_and_rejects_address_injection() {
        let temporary = tempfile::TempDir::new().unwrap();
        let path = temporary.path().join("notification.json");
        let valid = r#"{"relay_host":"smtp-relay","relay_port":25,"from_address":"bokkie@example.org","destination":"reader+reminders@example.org","timeout_ms":1000}"#;
        std::fs::write(&path, valid).unwrap();
        assert_eq!(
            NotificationConfig::load(&path).unwrap().destination(),
            "reader+reminders@example.org"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), valid);
        assert!(NotificationConfig::load(Path::new("relative.json")).is_err());
        assert!(NotificationConfig::load(temporary.path()).is_err());
        std::fs::write(
            &path,
            valid.replace("1000}", "1000,\"password\":\"unsupported\"}"),
        )
        .unwrap();
        assert!(NotificationConfig::load(&path).is_err());
        std::fs::write(&path, " ".repeat(8193)).unwrap();
        assert!(NotificationConfig::load(&path).is_err());
        for address in [
            "reader@example.org\r\nRCPT TO:<other@example.org>",
            "reader@example.org,other@example.org",
            "Reader <reader@example.org>",
            "réader@example.org",
            "reader@-example.org",
            "reader..name@example.org",
        ] {
            assert!(validate_address(address).is_err(), "{address:?}");
        }
    }
}
