//! The gateway's outbound HTTP client, shared by every tool and service
//! that reaches the web, such as `web_fetch` and Web Push.
//!
//! [`HttpClient::send`] makes one HTTP/1.1 request, over TLS for `https`,
//! and never follows a redirect: a caller that follows one sends the next
//! request itself, so every hop passes the same checks. Each request:
//!
//! - resolves its host itself, and refuses it unless every address the
//!   name resolves to is public ([`address::is_public`]): no loopback,
//!   private, link-local, tailnet, unique local, multicast or other
//!   reserved address, IPv4-mapped IPv6 included;
//! - connects to one of those checked addresses and no other, so a name
//!   that resolves differently later (DNS rebinding) cannot move the
//!   connection; TLS and `Host` still use the original name;
//! - trusts the system's certificate authorities (the image's
//!   ca-certificates), with rustls on ring;
//! - sends a fixed `User-Agent` and no cookie or credential of its own;
//! - reads at most the caller's limit of the response body.
//!
//! The caller sets the deadline, by dropping the future.
//!
//! The [`Network`] decides how names resolve and which addresses pass.
//! In production it is always the system's resolver with the public-only
//! rule. With the `test-network` feature, which only this crate's own
//! tests turn on, a test can give it fixed answers, let loopback through
//! and trust a test certificate authority, so that tests reach local
//! servers without touching the internet.

pub mod address;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::rustls;
use url::{Host, Url};

/// The `User-Agent` of every request the gateway makes.
pub const USER_AGENT: &str = concat!(
    "Bokkie/",
    env!("CARGO_PKG_VERSION"),
    " (private household assistant; +https://github.com/robchristie/bokkie)"
);

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("{0}")]
    InvalidUrl(String),
    /// The host, or one of the addresses its name resolves to, is not a
    /// public address.
    #[error("{0} is not a public address")]
    NotPublic(String),
    #[error("could not resolve {0}")]
    Resolve(String),
    #[error("could not connect to {0}")]
    Connect(String),
    #[error("a secure connection to {0} failed: {1}")]
    Tls(String, String),
    #[error("the exchange with {0} failed: {1}")]
    Exchange(String, String),
}

/// One request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: Url,
    /// Further headers. `Host` and `User-Agent` are the client's own, and
    /// any given here are dropped.
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Bytes,
}

impl Request {
    /// A `GET` of `url` with no further headers.
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::GET,
            url,
            headers: Vec::new(),
            body: Bytes::new(),
        }
    }
}

/// A response, with at most the limit of its body.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// Whether the body went on past the limit.
    pub truncated: bool,
    /// The address the request went to.
    pub address: SocketAddr,
}

/// How names resolve and which addresses the client may reach.
#[derive(Debug, Clone, Default)]
pub struct Network {
    #[cfg(feature = "test-network")]
    test: Option<Arc<test_network::TestNetwork>>,
}

impl Network {
    /// The system's resolver, public addresses only. Production uses
    /// nothing else.
    pub fn system() -> Self {
        Self::default()
    }

    /// Whether the client may connect to `address`.
    pub fn allows(&self, address: IpAddr) -> bool {
        #[cfg(feature = "test-network")]
        if let Some(test) = &self.test
            && address.is_loopback()
        {
            return test.allow_loopback;
        }
        address::is_public(address)
    }

    /// Every address `host` resolves to.
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, HttpError> {
        #[cfg(feature = "test-network")]
        if let Some(test) = &self.test {
            return test
                .resolve(host)
                .ok_or_else(|| HttpError::Resolve(host.to_owned()));
        }
        let found = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| HttpError::Resolve(host.to_owned()))?;
        let mut addresses: Vec<IpAddr> = Vec::new();
        for found in found {
            if !addresses.contains(&found.ip()) {
                addresses.push(found.ip());
            }
        }
        Ok(addresses)
    }

    /// The port to connect to for a URL's `port`.
    fn connect_port(&self, port: u16) -> u16 {
        #[cfg(feature = "test-network")]
        if let Some(test) = &self.test
            && let Some(mapped) = test.ports.get(&port)
        {
            return *mapped;
        }
        port
    }

    /// Certificate authorities trusted beside the system's.
    fn extra_roots(&self) -> Vec<rustls::pki_types::CertificateDer<'static>> {
        #[cfg(feature = "test-network")]
        if let Some(test) = &self.test {
            return test.roots.clone();
        }
        Vec::new()
    }
}

/// The gateway's HTTP client.
#[derive(Debug)]
pub struct HttpClient {
    network: Network,
    /// Built on first use: reading the system's certificates takes time.
    tls: OnceLock<Result<Arc<rustls::ClientConfig>, String>>,
}

impl HttpClient {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            tls: OnceLock::new(),
        }
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    fn tls(&self) -> Result<Arc<rustls::ClientConfig>, String> {
        self.tls
            .get_or_init(|| {
                let mut roots = rustls::RootCertStore::empty();
                let native = rustls_native_certs::load_native_certs();
                let (added, _) = roots.add_parsable_certificates(native.certs);
                let (extra, _) = roots.add_parsable_certificates(self.network.extra_roots());
                if added + extra == 0 {
                    tracing::error!(errors = ?native.errors, "no trusted certificate authorities");
                    return Err("no certificate authority is trusted".into());
                }
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let mut config = rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .map_err(|e| e.to_string())?
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                config.alpn_protocols = vec![b"http/1.1".to_vec()];
                Ok(Arc::new(config))
            })
            .clone()
    }

    /// The checked addresses of `url`'s host, which must all be public.
    pub async fn addresses(&self, url: &Url) -> Result<Vec<IpAddr>, HttpError> {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| HttpError::InvalidUrl("the link has no port".into()))?;
        let host = url
            .host()
            .ok_or_else(|| HttpError::InvalidUrl("the link has no host".into()))?;
        let addresses = match host {
            Host::Ipv4(v4) => vec![IpAddr::V4(v4)],
            Host::Ipv6(v6) => vec![IpAddr::V6(v6)],
            Host::Domain(name) => self.network.resolve(name, port).await?,
        };
        if addresses.is_empty() {
            return Err(HttpError::Resolve(host.to_string()));
        }
        // One non-public answer refuses the name: the connection must not
        // depend on which answer is tried.
        if addresses.iter().any(|a| !self.network.allows(*a)) {
            return Err(HttpError::NotPublic(host.to_string()));
        }
        Ok(addresses)
    }

    /// Makes one request and reads at most `limit` bytes of its body.
    pub async fn send(&self, request: Request, limit: usize) -> Result<Response, HttpError> {
        let url = &request.url;
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            other => {
                return Err(HttpError::InvalidUrl(format!(
                    "{other} links are not supported"
                )));
            }
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(HttpError::InvalidUrl(
                "links with a user name or password are not supported".into(),
            ));
        }
        let host = url
            .host()
            .ok_or_else(|| HttpError::InvalidUrl("the link has no host".into()))?
            .to_owned();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| HttpError::InvalidUrl("the link has no port".into()))?;
        let addresses = self.addresses(url).await?;

        // Connect to a checked address, and only to one.
        let mut stream = None;
        for ip in &addresses {
            let target = SocketAddr::new(*ip, self.network.connect_port(port));
            if let Ok(connected) = TcpStream::connect(target).await {
                stream = Some((connected, target));
                break;
            }
        }
        let Some((stream, address)) = stream else {
            return Err(HttpError::Connect(host.to_string()));
        };
        let _ = stream.set_nodelay(true);

        // `Host` is the original name, with the port when it is not the
        // scheme's default.
        let host_header = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        let mut builder = hyper::Request::builder()
            .method(request.method.clone())
            .uri(path)
            .header(header::HOST, host_header)
            .header(header::USER_AGENT, USER_AGENT);
        for (name, value) in &request.headers {
            if name != header::HOST && name != header::USER_AGENT {
                builder = builder.header(name, value);
            }
        }
        let outgoing = builder
            .body(Full::new(request.body.clone()))
            .map_err(|e| HttpError::InvalidUrl(e.to_string()))?;

        let name = host.to_string();
        if https {
            let config = self.tls().map_err(|e| HttpError::Tls(name.clone(), e))?;
            let server_name = match &host {
                Host::Domain(domain) => rustls::pki_types::ServerName::try_from(domain.clone())
                    .map_err(|e| HttpError::Tls(name.clone(), e.to_string()))?,
                Host::Ipv4(v4) => rustls::pki_types::ServerName::IpAddress((*v4).into()),
                Host::Ipv6(v6) => rustls::pki_types::ServerName::IpAddress((*v6).into()),
            };
            let tls = tokio_rustls::TlsConnector::from(config)
                .connect(server_name, stream)
                .await
                .map_err(|e| HttpError::Tls(name.clone(), e.to_string()))?;
            exchange(TokioIo::new(tls), outgoing, limit, &name, address).await
        } else {
            exchange(TokioIo::new(stream), outgoing, limit, &name, address).await
        }
    }
}

/// Sends the request over a connected stream and reads the response, with
/// at most `limit` bytes of its body. The connection is driven here, not
/// in a task of its own, so dropping the future closes it.
async fn exchange<S>(
    io: TokioIo<S>,
    request: hyper::Request<Full<Bytes>>,
    limit: usize,
    name: &str,
    address: SocketAddr,
) -> Result<Response, HttpError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let failed = |e: &dyn std::fmt::Display| HttpError::Exchange(name.to_owned(), e.to_string());
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| failed(&e))?;
    let work = async {
        let response = sender.send_request(request).await.map_err(|e| failed(&e))?;
        let (parts, mut body) = response.into_parts();
        let mut read = Vec::new();
        let mut truncated = false;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| failed(&e))?;
            if let Ok(data) = frame.into_data() {
                let room = limit - read.len();
                if data.len() > room {
                    read.extend_from_slice(&data[..room]);
                    truncated = true;
                    break;
                }
                read.extend_from_slice(&data);
            }
        }
        Ok(Response {
            status: parts.status,
            headers: parts.headers,
            body: Bytes::from(read),
            truncated,
            address,
        })
    };
    let mut work = std::pin::pin!(work);
    let mut connection = std::pin::pin!(connection);
    let mut connection_done = false;
    loop {
        tokio::select! {
            result = &mut work => return result,
            // The connection can finish first, having delivered the whole
            // response; the body then still reads to its end.
            _ = &mut connection, if !connection_done => connection_done = true,
        }
    }
}

#[cfg(feature = "test-network")]
pub mod test_network {
    //! A network for tests: fixed answers for the names a test uses, and
    //! nothing else; optionally loopback let through, ports mapped to
    //! local servers, and a test certificate authority trusted.
    //!
    //! Only the `test-network` feature builds this, and only this crate's
    //! tests turn the feature on.

    use std::collections::{HashMap, VecDeque};
    use std::net::IpAddr;
    use std::sync::{Arc, Mutex};

    use tokio_rustls::rustls::pki_types::CertificateDer;

    use super::Network;

    #[derive(Debug, Default)]
    pub struct TestNetwork {
        /// Each name's answers, in turn: the last one repeats.
        answers: Mutex<HashMap<String, VecDeque<Vec<IpAddr>>>>,
        lookups: Mutex<Vec<String>>,
        pub(super) allow_loopback: bool,
        pub(super) ports: HashMap<u16, u16>,
        pub(super) roots: Vec<CertificateDer<'static>>,
    }

    impl TestNetwork {
        pub fn new() -> Self {
            Self::default()
        }

        /// Lets loopback addresses through, as if public.
        pub fn allow_loopback(mut self) -> Self {
            self.allow_loopback = true;
            self
        }

        /// Connects to `to` where a URL names port `from`.
        pub fn map_port(mut self, from: u16, to: u16) -> Self {
            self.ports.insert(from, to);
            self
        }

        /// Trusts a certificate authority, in DER.
        pub fn trust(mut self, der: Vec<u8>) -> Self {
            self.roots.push(CertificateDer::from(der));
            self
        }

        /// `name` resolves to `addresses`, after any answers given before.
        pub fn answer(&self, name: &str, addresses: &[&str]) {
            let parsed = addresses.iter().map(|a| a.parse().unwrap()).collect();
            self.answers
                .lock()
                .unwrap()
                .entry(name.to_owned())
                .or_default()
                .push_back(parsed);
        }

        /// The names looked up so far, in order.
        pub fn lookups(&self) -> Vec<String> {
            self.lookups.lock().unwrap().clone()
        }

        pub(super) fn resolve(&self, name: &str) -> Option<Vec<IpAddr>> {
            self.lookups.lock().unwrap().push(name.to_owned());
            let mut answers = self.answers.lock().unwrap();
            let queue = answers.get_mut(name)?;
            if queue.len() > 1 {
                queue.pop_front()
            } else {
                queue.front().cloned()
            }
        }

        pub fn network(self: &Arc<Self>) -> Network {
            Network {
                test: Some(self.clone()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_is_fixed_and_identifies_bokkie() {
        assert!(USER_AGENT.starts_with("Bokkie/"));
        assert!(HeaderValue::from_static(USER_AGENT).to_str().is_ok());
    }

    #[test]
    fn the_system_network_allows_only_public_addresses() {
        let network = Network::system();
        assert!(network.allows("93.184.216.34".parse().unwrap()));
        assert!(!network.allows("127.0.0.1".parse().unwrap()));
        assert!(!network.allows("::1".parse().unwrap()));
        assert!(!network.allows("10.0.0.1".parse().unwrap()));
        assert_eq!(network.connect_port(443), 443);
        assert!(network.extra_roots().is_empty());
    }

    #[tokio::test]
    async fn literal_and_unsupported_hosts_are_refused_before_connecting() {
        let client = HttpClient::new(Network::system());
        for (url, expected) in [
            ("http://127.0.0.1/", "127.0.0.1 is not a public address"),
            ("http://[::1]/", "[::1] is not a public address"),
            ("http://[::ffff:10.0.0.1]/", "is not a public address"),
            ("http://169.254.169.254/latest", "is not a public address"),
            ("http://0x7f000001/", "is not a public address"),
            ("http://2130706433/", "is not a public address"),
            ("ftp://example.org/", "ftp links are not supported"),
            ("https://user:pw@example.org/", "user name or password"),
        ] {
            let error = client
                .send(Request::get(url.parse().unwrap()), 10)
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{url}: {error}");
        }
    }
}
