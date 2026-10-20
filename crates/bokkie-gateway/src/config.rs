//! Gateway settings, read from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::web::WebRoot;

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
    /// The pinned `codex` binary.
    pub codex_program: PathBuf,
    /// The production web package's application directory, served at the
    /// origin's root. `None` serves no static files.
    pub web_root: Option<WebRoot>,
    /// The contact a push service may reach the operator at, named in
    /// every Web Push request. Without it the gateway serves its push key
    /// but sends no push.
    pub push_contact: Option<PushContact>,
}

/// The operator's contact for push services: an `https:` URL or a
/// `mailto:` address, of printable ASCII.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushContact(String);

impl PushContact {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        let refuse = || {
            format!(
                "BOKKIE_PUSH_CONTACT {value:?} must be an https: URL, such as \
                 https://bokkie.example.lan, or a mailto: address"
            )
        };
        if value.len() > 256 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(refuse());
        }
        if let Some(address) = value.strip_prefix("mailto:") {
            let (local, domain) = address.split_once('@').ok_or_else(refuse)?;
            if local.is_empty() || !valid_host(&domain.to_ascii_lowercase()) || domain.contains('[')
            {
                return Err(refuse());
            }
        } else {
            let url = url::Url::parse(value).map_err(|_| refuse())?;
            if url.scheme() != "https"
                || url.host_str().is_none_or(str::is_empty)
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(refuse());
            }
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An `https://host[:port]` or `http://host[:port]` origin, normalised to
/// lower case without a trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin {
    origin: String,
    authority: String,
}

impl PublicOrigin {
    /// Accepts only a host and port that are safe in a header and could
    /// match what browsers send in `Origin` and `Host`: an ASCII host name
    /// (punycode for an international one) or IPv4 address of letters,
    /// digits, `-`, `_` and dots, or a bracketed IPv6 address, and an
    /// optional port other than the scheme's default, without leading zeros.
    /// It does not canonicalise addresses: write one as browsers do, such as
    /// `127.0.0.1` rather than `127.1`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let origin = value.trim().trim_end_matches('/').to_ascii_lowercase();
        let (authority, default_port) = match origin.strip_prefix("https://") {
            Some(authority) => (authority, "443"),
            None => match origin.strip_prefix("http://") {
                Some(authority) => (authority, "80"),
                None => {
                    return Err(format!(
                        "origin {value:?} must start with https:// or http://"
                    ));
                }
            },
        };
        let (host, port) = match authority.rsplit_once(':') {
            // A colon inside the brackets belongs to an IPv6 address.
            Some((host, port)) if !port.contains(']') => (host, Some(port)),
            _ => (authority, None),
        };
        if !valid_host(host) {
            return Err(format!(
                "origin {value:?} must be scheme://host[:port], with an ASCII host name, \
                 an IPv4 address or a bracketed IPv6 address"
            ));
        }
        if let Some(port) = port
            && !(port
                .parse::<u16>()
                .is_ok_and(|n| n != 0 && n.to_string() == port)
                && port != default_port)
        {
            return Err(format!(
                "origin {value:?} must have a port from 1 to 65535 without leading zeros, \
                 and omit the scheme's default port"
            ));
        }
        let authority = authority.to_owned();
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

/// Whether `host`, already lower case, is a host name of letters, digits,
/// `-` and `_` in non-empty dot-separated labels (which covers an IPv4
/// address), or an IPv6 address in brackets.
fn valid_host(host: &str) -> bool {
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(address) => address.parse::<std::net::Ipv6Addr>().is_ok(),
        None => host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        }),
    }
}

impl Config {
    /// Reads `BOKKIE_DATA` (default `/data`), `BOKKIE_LISTEN` (default
    /// `0.0.0.0:8080`), `BOKKIE_ORIGIN` (required), `BOKKIE_EXTRA_HOSTS`
    /// (comma-separated), `BOKKIE_TRUST_FORWARDED_FOR` (`1` to enable),
    /// `BOKKIE_CODEX_BIN` (default `/usr/local/bin/codex`) and
    /// `BOKKIE_WEB_ROOT` (unset by default), which must name a readable
    /// directory holding `index.html`, and `BOKKIE_PUSH_CONTACT` (unset by
    /// default; see [`PushContact`]).
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let origin = var("BOKKIE_ORIGIN").ok_or("BOKKIE_ORIGIN is required")?;
        Ok(Self {
            data_dir: Self::data_dir_from_env(),
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
            codex_program: var("BOKKIE_CODEX_BIN")
                .unwrap_or_else(|| "/usr/local/bin/codex".into())
                .into(),
            web_root: var("BOKKIE_WEB_ROOT")
                .map(|root| WebRoot::open(root.as_ref()))
                .transpose()?,
            push_contact: var("BOKKIE_PUSH_CONTACT")
                .map(|contact| PushContact::parse(&contact))
                .transpose()?,
        })
    }

    /// `BOKKIE_DATA`, or `/data`: all an operator's command needs.
    pub fn data_dir_from_env() -> PathBuf {
        std::env::var("BOKKIE_DATA")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "/data".into())
            .into()
    }

    /// The database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("bokkie.sqlite3")
    }

    /// Bokkie's own model catalog, shared by every app server.
    pub fn catalog_path(&self) -> PathBuf {
        self.data_dir.join("codex").join("models.json")
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

    #[test]
    fn hosts_and_ports_browsers_send_unchanged_are_accepted() {
        for (value, authority) in [
            ("https://bokkie-next.yutani.tech", "bokkie-next.yutani.tech"),
            ("http://localhost:8787", "localhost:8787"),
            ("http://127.0.0.1:8080", "127.0.0.1:8080"),
            ("https://[::1]:8443", "[::1]:8443"),
            ("https://[FD00::1]", "[fd00::1]"),
            ("https://xn--bcher-kva.example", "xn--bcher-kva.example"),
            ("https://my_host.lan:65535", "my_host.lan:65535"),
            ("http://x:443", "x:443"),
        ] {
            let origin = PublicOrigin::parse(value).unwrap();
            assert_eq!(origin.authority(), authority, "{value}");
        }
    }

    #[test]
    fn invalid_hosts_and_ports_are_configuration_errors() {
        for bad in [
            // Control characters, which no header value may carry.
            "https://bokkie\u{7}.lan",
            "https://bokkie.lan\u{0}",
            "https://bok\tkie.lan",
            "https://bokkie.lan\r\nx-injected: 1",
            "https://bokkie.lan:\u{1b}80",
            "https://bokkie.lan\u{7f}",
            // Other characters invalid in a host.
            "https://bok kie.lan",
            "https://bokkie.lan;",
            "https://\"bokkie\".lan",
            "https://bokkie.lan\\x",
            "https://bücher.example",
            "https://bokkie..lan",
            "https://.bokkie.lan",
            "https://bokkie.lan.",
            "https://bokkie%2elan",
            // Ports.
            "https://bokkie.lan:",
            "https://bokkie.lan:0",
            "https://bokkie.lan:65536",
            "https://bokkie.lan:08443",
            "https://bokkie.lan:+8443",
            "https://bokkie.lan:84a3",
            "https://bokkie.lan:443",
            "http://bokkie.lan:80",
            "https://bokkie.lan:1:2",
            // IPv6.
            "https://::1",
            "https://[::1",
            "https://[::1]x",
            "https://[::g]",
            "https://[fe80::1%eth0]",
            "https://[]",
        ] {
            assert!(PublicOrigin::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_push_contact_is_an_https_url_or_a_mailto_address() {
        for good in [
            "https://bokkie-next.yutani.tech",
            "https://bokkie.example.lan/contact",
            "mailto:owner@example.org",
        ] {
            assert_eq!(PushContact::parse(good).unwrap().as_str(), good);
        }
        for bad in [
            "",
            "bokkie.example.lan",
            "http://bokkie.example.lan",
            "https://",
            "https://user:pw@bokkie.example.lan",
            "https://bokkie example.lan",
            "https://bokkie.lan\r\nx: 1",
            "https://bökkie.lan",
            "mailto:",
            "mailto:owner",
            "mailto:@example.org",
            "mailto:owner@",
            "mailto:owner@exa mple.org",
            "ftp://bokkie.example.lan",
        ] {
            assert!(PushContact::parse(bad).is_err(), "{bad:?}");
        }
        let long = format!("https://bokkie.lan/{}", "a".repeat(300));
        assert!(PushContact::parse(&long).is_err());
    }

    #[test]
    fn every_accepted_origin_makes_a_content_security_policy() {
        // A policy is built from the origin at start-up and must not panic;
        // try each printable and control character in the host and the port.
        for c in (0u8..=0x7f)
            .map(char::from)
            .chain(['é', '\u{85}', '\u{2028}'])
        {
            for value in [
                format!("https://bok{c}kie.lan"),
                format!("https://bokkie.lan:8{c}"),
            ] {
                if let Ok(origin) = PublicOrigin::parse(&value) {
                    crate::web::ContentSecurityPolicy::for_origin(&origin);
                }
            }
        }
    }
}
