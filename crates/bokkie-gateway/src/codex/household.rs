//! The household's ChatGPT sign-in, which every app server shares.
//!
//! One ChatGPT sign-in serves the whole household. It lives in one file,
//! `<data>/household/codex/auth.json`, held by the household app server,
//! which runs no threads and is the only app server that signs in,
//! refreshes or signs out. Each user's `<data>/users/<id>/codex/auth.json`
//! is a symbolic link to it ([`LINK_TARGET`]), so every app server reads
//! the same tokens and a refresh by the household reaches them all.
//!
//! Codex refreshes an access token itself once it is within five minutes
//! of expiry. Refresh tokens are single-use, and Codex has no lock across
//! processes, so two app servers refreshing at once could leave one of them
//! signed out. The gateway therefore refreshes the household's token well
//! before that ([`HouseholdSettings::refresh_ahead`]); a member app server
//! whose cached token goes stale then finds a fresh one on disk and reloads
//! it without refreshing. To schedule this the gateway reads one claim of
//! the household's access token, its expiry, and nothing else.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use bokkie_protocol::Timestamp;
use bokkie_protocol::api::{DeviceCodeLogin, LoginState};

/// Where each user's `auth.json` points, relative to the user's
/// `CODEX_HOME`: the household's `auth.json`. Relative, so it survives the
/// data directory moving.
pub const LINK_TARGET: &str = "../../../household/codex/auth.json";

/// How long a device code works: Codex 0.163.0 waits 15 minutes for one to
/// be entered (`codex-rs/login/src/device_code_auth.rs`).
pub const DEVICE_CODE_LIFETIME_MS: i64 = 15 * 60 * 1000;

/// When the gateway refreshes the household's sign-in.
#[derive(Debug, Clone)]
pub struct HouseholdSettings {
    /// How long before the access token expires the gateway refreshes it.
    /// Codex refreshes 5 minutes before expiry; this must be well before
    /// that, so that a member's app server always finds a fresh token on
    /// disk. It assumes access tokens live much longer than this.
    pub refresh_ahead: Duration,
    /// How often the refresher looks at the token's expiry.
    pub check_every: Duration,
    /// How long after a failed refresh the gateway waits before trying
    /// again, so a failing sign-in is not retried on every request.
    pub retry_after: Duration,
}

impl Default for HouseholdSettings {
    fn default() -> Self {
        Self {
            refresh_ahead: Duration::from_secs(15 * 60),
            check_every: Duration::from_secs(30),
            retry_after: Duration::from_secs(60),
        }
    }
}

/// What an app server's `auth.json` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthFile {
    Missing,
    /// The link to the household's file, exactly.
    Link,
    /// A regular file: for a user, their own earlier sign-in.
    File,
    /// Anything else, described.
    Other(String),
}

/// Looks at `path` without following a link.
pub fn inspect(path: &Path) -> std::io::Result<AuthFile> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthFile::Missing);
        }
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        return Ok(if target == Path::new(LINK_TARGET) {
            AuthFile::Link
        } else {
            AuthFile::Other("a symbolic link to somewhere else".into())
        });
    }
    if metadata.is_file() {
        return Ok(AuthFile::File);
    }
    Ok(AuthFile::Other("neither a file nor a link".into()))
}

/// Makes `path` the link to the household's file, replacing nothing: the
/// caller has made sure there is nothing there.
pub fn make_link(path: &Path) -> std::io::Result<()> {
    let temporary: PathBuf = path.with_extension("link");
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(LINK_TARGET, &temporary)?;
    std::fs::rename(&temporary, path)
}

/// The expiry, in Unix seconds, of the access token in the household's
/// `auth.json`, or `None` when there is no sign-in there. Reads that one
/// claim only. Codex rewrites the file in place, so a read that finds it
/// half-written is tried again.
pub async fn access_token_expiry(path: &Path) -> Option<i64> {
    for attempt in 0..3 {
        match std::fs::read_to_string(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Ok(text) => {
                if let Some(expiry) = parse_expiry(&text) {
                    return Some(expiry);
                }
            }
            Err(_) => {}
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    tracing::warn!("the household's auth.json holds no readable access token");
    None
}

fn parse_expiry(text: &str) -> Option<i64> {
    let file: serde_json::Value = serde_json::from_str(text).ok()?;
    let token = file["tokens"]["access_token"].as_str()?;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims["exp"].as_i64()
}

/// Seconds since the Unix epoch, by the system clock, which is what token
/// expiries are measured against.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// A device-code sign-in of the household app server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub login_id: String,
    pub verification_url: String,
    pub user_code: String,
    pub state: LoginState,
    pub expires_at: Timestamp,
    pub error: Option<String>,
}

impl Login {
    pub fn to_api(&self) -> DeviceCodeLogin {
        DeviceCodeLogin {
            verification_url: self.verification_url.clone(),
            user_code: self.user_code.clone(),
            state: self.state,
            expires_at: self.expires_at,
            error: self.error.clone(),
        }
    }

    /// Records the end Codex reported for this sign-in, unless it already
    /// ended (a cancelled one reports failure afterwards).
    pub fn completed(&mut self, success: bool, error: Option<String>) {
        if self.state != LoginState::Pending {
            return;
        }
        if success {
            self.state = LoginState::Completed;
            return;
        }
        let error = error.unwrap_or_else(|| "The sign-in did not complete".into());
        // Codex ends an unused device code with "device auth timed out".
        self.state = if error.contains("timed out") {
            LoginState::Expired
        } else {
            LoginState::Failed
        };
        // Bounded: Codex's message is shown to admins as it is.
        self.error = Some(error.chars().take(300).collect());
    }

    /// Marks a pending sign-in expired once its code's time has passed.
    pub fn expire(&mut self, now: Timestamp) {
        if self.state == LoginState::Pending && now >= self.expires_at {
            self.state = LoginState::Expired;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(claims: &serde_json::Value) -> String {
        let encode = |v: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!(
            "{}.{}.sig",
            encode(&serde_json::json!({"alg": "none"})),
            encode(claims)
        )
    }

    #[test]
    fn only_the_access_tokens_expiry_is_read() {
        let file = serde_json::json!({"tokens": {
            "access_token": token(&serde_json::json!({"exp": 1_900_000_000})),
            "id_token": "not even a token",
            "refresh_token": "r",
        }});
        assert_eq!(parse_expiry(&file.to_string()), Some(1_900_000_000));
        assert_eq!(parse_expiry("{\"tokens\": {"), None, "half-written");
        assert_eq!(parse_expiry("{}"), None);
        let no_exp = serde_json::json!({"tokens": {"access_token": token(&serde_json::json!({}))}});
        assert_eq!(parse_expiry(&no_exp.to_string()), None);
    }

    #[test]
    fn links_are_told_apart_from_files_and_other_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        assert_eq!(inspect(&path).unwrap(), AuthFile::Missing);
        make_link(&path).unwrap();
        assert_eq!(inspect(&path).unwrap(), AuthFile::Link);
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            PathBuf::from(LINK_TARGET)
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/elsewhere/auth.json", &path).unwrap();
        assert!(matches!(inspect(&path).unwrap(), AuthFile::Other(_)));
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(inspect(&path).unwrap(), AuthFile::File);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(inspect(&path).unwrap(), AuthFile::Other(_)));
    }

    #[test]
    fn a_login_ends_once() {
        let mut login = Login {
            login_id: "l".into(),
            verification_url: "https://example.test/device".into(),
            user_code: "ABCD".into(),
            state: LoginState::Pending,
            expires_at: Timestamp(1000),
            error: None,
        };
        login.expire(Timestamp(999));
        assert_eq!(login.state, LoginState::Pending);
        let mut expired = login.clone();
        expired.completed(false, Some("device auth timed out after 15 minutes".into()));
        assert_eq!(expired.state, LoginState::Expired);
        let mut failed = login.clone();
        failed.completed(false, Some("the authority refused".into()));
        assert_eq!(failed.state, LoginState::Failed);
        failed.completed(true, None);
        assert_eq!(
            failed.state,
            LoginState::Failed,
            "a later report changes nothing"
        );
        login.expire(Timestamp(1000));
        assert_eq!(login.state, LoginState::Expired);
    }
}
