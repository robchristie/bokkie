//! Gateway settings, read from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

/// The gateway's settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the database and every user's directories live.
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// The one origin clients use, such as `https://bokkie.example.lan`.
    /// State-changing requests and the event WebSocket must carry it as
    /// `Origin`, and every request's `Host` must be its authority.
    pub origin: PublicOrigin,
    /// Further `Host` values to accept, such as a tailnet name that the
    /// reverse proxy forwards unchanged. Requests with these hosts must still
    /// carry [`Config::origin`] as their `Origin`.
    pub extra_hosts: Vec<String>,
    /// Use the last `X-Forwarded-For` entry, which the reverse proxy appends,
    /// as the client address for sign-in rate limiting. Enable only behind a
    /// proxy that sets it.
    pub trust_forwarded_for: bool,
}

/// An `https://host[:port]` or `http://host[:port]` origin, normalised to
/// lower case without a trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin {
    origin: String,
    authority: String,
}

impl PublicOrigin {
    pub fn parse(value: &str) -> Result<Self, String> {
        let origin = value.trim().trim_end_matches('/').to_ascii_lowercase();
        let authority = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"))
            .ok_or_else(|| format!("origin {value:?} must start with https:// or http://"))?
            .to_owned();
        if authority.is_empty() || authority.contains(['/', '?', '#', '@', ' ']) {
            return Err(format!("origin {value:?} must be scheme://host[:port]"));
        }
        Ok(Self { origin, authority })
    }

    /// The origin as browsers send it in the `Origin` header.
    pub fn as_str(&self) -> &str {
        &self.origin
    }

    /// The `host[:port]` part, as browsers send it in the `Host` header.
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

impl Config {
    /// Reads `BOKKIE_DATA` (default `/data`), `BOKKIE_LISTEN` (default
    /// `0.0.0.0:8080`), `BOKKIE_ORIGIN` (required), `BOKKIE_EXTRA_HOSTS`
    /// (comma-separated) and `BOKKIE_TRUST_FORWARDED_FOR` (`1` to enable).
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let origin = var("BOKKIE_ORIGIN").ok_or("BOKKIE_ORIGIN is required")?;
        Ok(Self {
            data_dir: var("BOKKIE_DATA").unwrap_or_else(|| "/data".into()).into(),
            listen: var("BOKKIE_LISTEN")
                .unwrap_or_else(|| "0.0.0.0:8080".into())
                .parse()
                .map_err(|e| format!("BOKKIE_LISTEN: {e}"))?,
            origin: PublicOrigin::parse(&origin)?,
            extra_hosts: var("BOKKIE_EXTRA_HOSTS")
                .map(|hosts| {
                    hosts
                        .split(',')
                        .map(|h| h.trim().to_ascii_lowercase())
                        .filter(|h| !h.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            trust_forwarded_for: var("BOKKIE_TRUST_FORWARDED_FOR").as_deref() == Some("1"),
        })
    }

    /// The database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("bokkie.sqlite3")
    }

    /// Whether `host` (a `Host` header value) names this gateway.
    pub fn accepts_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        host == self.origin.authority() || self.extra_hosts.contains(&host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_normalised() {
        let origin = PublicOrigin::parse("HTTPS://Bokkie.Example.lan:8443/").unwrap();
        assert_eq!(origin.as_str(), "https://bokkie.example.lan:8443");
        assert_eq!(origin.authority(), "bokkie.example.lan:8443");
        for bad in [
            "bokkie.lan",
            "ftp://x",
            "https://",
            "https://x/path",
            "https://u@x",
        ] {
            assert!(PublicOrigin::parse(bad).is_err(), "{bad}");
        }
    }
}
