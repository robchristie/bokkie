//! Encrypted Web Push with one bounded HTTPS POST per durable delivery attempt.
//! Provider acceptance is distinct from a service-worker receipt or display.
use std::{
    fmt,
    io::{self, Read},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    os::fd::AsRawFd,
    path::Path,
    sync::{Arc, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openssl::{
    bn::BigNumContext,
    ec::{EcGroup, EcPoint},
    nid::Nid,
    ssl::{ErrorCode, HandshakeError, SslConnector, SslMethod, SslStream},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::io::Write;
use web_push::{SubscriptionInfo, VapidSignatureBuilder};

use super::{NotificationIntent, NotificationOutcome, NotificationSender};

const MAX_CONFIG_BYTES: usize = 8192;
const MAX_ENDPOINT_BYTES: usize = 2048;
const MAX_PLAINTEXT_BYTES: usize = 3200;
const MAX_ENCRYPTED_BYTES: usize = 4096;
const MAX_RESPONSE_HEADERS: usize = 8192;
const PUSH_PROFILE_PREFIX: &str = "reminder-web-push-v1/";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushConfig {
    pub vapid_private_key: String,
    pub subject: String,
    pub timeout_ms: u64,
    pub ttl_seconds: u32,
}
impl fmt::Debug for PushConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushConfig")
            .field("credentials", &"[redacted]")
            .field("timeout_ms", &self.timeout_ms)
            .field("ttl_seconds", &self.ttl_seconds)
            .finish()
    }
}
impl PushConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("push configuration requires an absolute regular JSON file".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| "push configuration could not be opened".to_string())?;
        if !file
            .metadata()
            .map_err(|_| "push configuration metadata unavailable")?
            .is_file()
        {
            return Err("push configuration must be a regular JSON file".into());
        }
        let mut raw = Vec::new();
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|_| "push configuration could not be read")?;
        if raw.len() > MAX_CONFIG_BYTES {
            return Err("push configuration exceeds 8192 bytes".into());
        }
        // Serde errors can contain supplied values: never expose their text.
        let value: Self = serde_json::from_slice(&raw)
            .map_err(|_| "push configuration requires the exact JSON schema")?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(100..=3000).contains(&self.timeout_ms) || !(60..=86400).contains(&self.ttl_seconds) {
            return Err(
                "push configuration requires timeout 100..=3000 ms and TTL 60..=86400 seconds"
                    .into(),
            );
        }
        let subject = clean_https_url(&self.subject)?;
        let host = subject.host;
        if self.subject.len() > 253
            || subject.path != "/"
            || !host.contains('.')
            || [
                ".localhost",
                ".local",
                ".internal",
                ".test",
                ".invalid",
                ".onion",
            ]
            .iter()
            .any(|suffix| host.ends_with(suffix))
        {
            return Err("VAPID subject must be a public HTTPS contact origin".into());
        }
        self.public_key()?;
        Ok(())
    }

    pub fn public_key(&self) -> Result<String, String> {
        decode_exact(&self.vapid_private_key, 32)
            .map_err(|_| "VAPID private key must be a raw 32-byte base64url scalar")?;
        let builder = VapidSignatureBuilder::from_base64_no_sub(&self.vapid_private_key)
            .map_err(|_| "VAPID private key is invalid")?;
        Ok(URL_SAFE_NO_PAD.encode(builder.get_public_key()))
    }
}

/// Saved device identity and key material; retries use this exact snapshot.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushSubscription {
    pub id: String,
    pub label: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub vapid_public_key: String,
}
impl fmt::Debug for PushSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushSubscription")
            .field("snapshot", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushIntent {
    pub subscription: PushSubscription,
    pub receipt_token: String,
    pub expires_at: i64,
}
impl fmt::Debug for PushIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushIntent")
            .field("snapshot", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub fn push_profile_id(id: &str) -> String {
    format!("{PUSH_PROFILE_PREFIX}{id}")
}

pub fn push_device_id(profile: &str) -> Option<&str> {
    let id = profile.strip_prefix(PUSH_PROFILE_PREFIX)?;
    canonical_uuid(id).then_some(id)
}

fn canonical_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
}

fn decode_exact(encoded: &str, size: usize) -> Result<Vec<u8>, String> {
    // Reject non-canonical and overlong encodings before allocation.
    if encoded.len() != (size * 8).div_ceil(6) {
        return Err("invalid base64url key length".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "invalid base64url key")?;
    if bytes.len() != size || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err("invalid base64url key".into());
    }
    Ok(bytes)
}

struct HttpsUrl<'a> {
    host: &'a str,
    path: &'a str,
}

fn clean_https_url(raw: &str) -> Result<HttpsUrl<'_>, String> {
    if raw.is_empty()
        || raw.len() > MAX_ENDPOINT_BYTES
        || !raw.is_ascii()
        || raw
            .bytes()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control() || c == b'\\')
        || raw.contains(['@', '?', '#'])
    {
        return Err("push address requires a bounded HTTPS URL without credentials".into());
    }
    let rest = raw
        .strip_prefix("https://")
        .ok_or("push address requires HTTPS port 443")?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    let path = if authority_end == rest.len() {
        "/"
    } else {
        &rest[authority_end..]
    };
    if host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || label
                    .bytes()
                    .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != b'-')
        })
    {
        return Err("push address has an invalid DNS hostname".into());
    }
    if host.parse::<IpAddr>().is_ok() {
        return Err("push address must use a DNS hostname".into());
    }
    Ok(HttpsUrl { host, path })
}

fn endpoint_url(endpoint: &str) -> Result<HttpsUrl<'_>, String> {
    let url = clean_https_url(endpoint)?;
    let host = url.host;
    if !(host == "fcm.googleapis.com"
        || host == "updates.push.services.mozilla.com"
        || host.ends_with(".push.apple.com"))
        || url.path == "/"
    {
        return Err(
            "push endpoint must name an allowed Apple, Google or Mozilla push service".into(),
        );
    }
    Ok(url)
}

pub fn validate_subscription(endpoint: &str, p256dh: &str, auth: &str) -> Result<(), String> {
    endpoint_url(endpoint)?;
    let public = decode_exact(p256dh, 65)?;
    decode_exact(auth, 16)?;
    if public[0] != 4 {
        return Err("push subscription requires an uncompressed P-256 key".into());
    }
    let valid = (|| {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
        let mut context = BigNumContext::new()?;
        let point = EcPoint::from_bytes(&group, &public, &mut context)?;
        Ok::<_, openssl::error::ErrorStack>(
            !point.is_infinity(&group) && point.is_on_curve(&group, &mut context)?,
        )
    })()
    .unwrap_or(false);
    if !valid {
        return Err("push subscription requires a valid P-256 public key".into());
    }
    Ok(())
}

pub fn payload(intent: &NotificationIntent) -> Result<Vec<u8>, String> {
    let push = intent
        .push
        .as_ref()
        .ok_or("delivery has no push snapshot")?;
    if !intent
        .id
        .strip_prefix("delivery-")
        .is_some_and(canonical_uuid)
        || !intent
            .task_id
            .strip_prefix("task-")
            .is_some_and(canonical_uuid)
    {
        return Err("push payload requires prefixed canonical delivery and task identities".into());
    }
    if push.receipt_token.len() != 64
        || !push
            .receipt_token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("push receipt token requires 64 lowercase hexadecimal characters".into());
    }
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "id": intent.id,
        "task_id": intent.task_id,
        "title": intent.subject,
        "body": intent.body,
        "receipt_token": push.receipt_token,
        "expires_at": push.expires_at,
    }))
    .map_err(|_| "push payload could not be encoded")?;
    if bytes.len() > MAX_PLAINTEXT_BYTES {
        return Err("push payload exceeds 3200 bytes".into());
    }
    Ok(bytes)
}

pub struct PushSender {
    config: Arc<PushConfig>,
    clock: Arc<dyn crate::UnixClock + Send + Sync>,
    tls: SslConnector,
    #[cfg(test)]
    fixture: Option<(SocketAddr, bool)>,
}
impl PushSender {
    pub fn new(
        config: Arc<PushConfig>,
        clock: Arc<dyn crate::UnixClock + Send + Sync>,
    ) -> Result<Self, String> {
        config.validate()?;
        let mut tls = SslConnector::builder(SslMethod::tls_client())
            .map_err(|_| "push TLS trust configuration unavailable")?;
        tls.set_min_proto_version(Some(openssl::ssl::SslVersion::TLS1_2))
            .map_err(|_| "push TLS policy unavailable")?;
        Ok(Self {
            config,
            clock,
            tls: tls.build(),
            #[cfg(test)]
            fixture: None,
        })
    }

    fn request(&self, intent: &NotificationIntent, now: i64) -> Result<Request, String> {
        let push = intent
            .push
            .as_ref()
            .ok_or("delivery has no push snapshot")?;
        validate_subscription(
            &push.subscription.endpoint,
            &push.subscription.p256dh,
            &push.subscription.auth,
        )?;
        if push.subscription.vapid_public_key != self.config.public_key()? {
            return Err("VAPID configuration differs from the saved device key; restore the reviewed key before retrying".into());
        }
        let remaining = push
            .expires_at
            .checked_sub(now)
            .filter(|seconds| *seconds > 0)
            .ok_or("push delivery has expired; the saved lifetime cannot be extended")?;
        let ttl = (remaining as u64).min(u64::from(self.config.ttl_seconds)) as u32;
        let endpoint = endpoint_url(&push.subscription.endpoint)?;
        let subscription = SubscriptionInfo::new(
            push.subscription.endpoint.clone(),
            push.subscription.p256dh.clone(),
            push.subscription.auth.clone(),
        );
        let mut vapid =
            VapidSignatureBuilder::from_base64(&self.config.vapid_private_key, &subscription)
                .map_err(|_| "push signing key is invalid")?;
        vapid.add_claim("sub", self.config.subject.clone());
        vapid.add_claim(
            "exp",
            now.checked_add(43200)
                .filter(|exp| *exp > 0)
                .ok_or("push clock is invalid")?,
        );
        let signature = vapid
            .build()
            .map_err(|_| "push authorisation could not be signed")?;
        let plaintext = payload(intent)?;
        // web-push 0.11's message builder imposes an unrelated 3052-byte cap.
        // Use its underlying RFC 8291 implementation for our 3200-byte contract.
        let encrypted = ece::encrypt(
            &decode_exact(&push.subscription.p256dh, 65)?,
            &decode_exact(&push.subscription.auth, 16)?,
            &plaintext,
        )
        .map_err(|_| "push payload could not be encrypted")?;
        if encrypted.len() > MAX_ENCRYPTED_BYTES {
            return Err("encrypted push payload exceeds 4096 bytes".into());
        }
        let host = endpoint.host.to_string();
        let topic = URL_SAFE_NO_PAD.encode(&Sha256::digest(intent.id.as_bytes())[..24]);
        let mut bytes = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: vapid t={}, k={}\r\nContent-Encoding: aes128gcm\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nTTL: {}\r\nTopic: {}\r\nConnection: close\r\n\r\n",
            endpoint.path, host, signature.auth_t, URL_SAFE_NO_PAD.encode(signature.auth_k), encrypted.len(), ttl, topic,
        ).into_bytes();
        bytes.extend(encrypted);
        Ok(Request { host, bytes, ttl })
    }
}

struct Request {
    host: String,
    bytes: Vec<u8>,
    ttl: u32,
}

impl NotificationSender for PushSender {
    fn reminder_modes(&self) -> (bool, bool) {
        (false, true)
    }

    fn send(&self, intent: &NotificationIntent) -> NotificationOutcome {
        let deadline = Instant::now() + Duration::from_millis(self.config.timeout_ms);
        let request = match self.request(intent, self.clock.now()) {
            Ok(request) => request,
            Err(detail) => {
                return NotificationOutcome::Rejected {
                    retryable: false,
                    detail,
                };
            }
        };
        let mut stream = match self.connect(&request.host, deadline) {
            Ok(stream) => stream,
            Err(_) => {
                return NotificationOutcome::Rejected {
                    retryable: true,
                    detail: "Push HTTPS connection failed before any POST was dispatched".into(),
                };
            }
        };
        // Once writing starts, loss of the final response cannot prove nonacceptance.
        let response = write_request(&mut stream, &request.bytes, deadline)
            .and_then(|()| read_response(&mut stream, deadline));
        match response {
            Ok(reply) if reply.code == 201 => NotificationOutcome::PushAccepted {
                detail: "Push service accepted the saved message; device receipt and display are not proved".into(),
                ttl_seconds: reply.ttl.unwrap_or(request.ttl).min(request.ttl),
            },
            Ok(reply) if reply.code == 404 || reply.code == 410 => NotificationOutcome::SubscriptionExpired {
                detail: format!("Push service returned {}; this saved device subscription has expired", reply.code),
            },
            Ok(reply) => NotificationOutcome::Rejected {
                retryable: reply.code == 429 || (500..600).contains(&reply.code),
                detail: format!("Push service returned {}; the message was not accepted", reply.code),
            },
            Err(_) => NotificationOutcome::Uncertain {
                detail: "Push POST may have been dispatched, but no valid final acceptance response was received; reconcile before resending".into(),
            },
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "push deadline elapsed",
        ))
    } else {
        Ok(duration)
    }
}

struct ResolveRequest {
    host: String,
    deadline: Instant,
    reply: mpsc::SyncSender<io::Result<Vec<SocketAddr>>>,
}
static RESOLVER: OnceLock<io::Result<mpsc::SyncSender<ResolveRequest>>> = OnceLock::new();

fn resolve(host: &str, deadline: Instant) -> io::Result<Vec<SocketAddr>> {
    let resolver = RESOLVER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ResolveRequest>(1);
            thread::Builder::new()
                .name("bokkie-push-resolver".into())
                .spawn(move || {
                    for request in receiver {
                        let result = remaining(request.deadline).and_then(|_| {
                            // A stalled OS resolver retains at most one worker and one queued
                            // request. It never connects, and expired replies cannot dispatch.
                            (request.host.as_str(), 443)
                                .to_socket_addrs()
                                .map(|addresses| addresses.take(17).collect())
                        });
                        let _ = request.reply.send(result);
                    }
                })?;
            Ok(sender)
        })
        .as_ref()
        .map_err(|_| io::Error::other("push resolver unavailable"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    resolver
        .try_send(ResolveRequest {
            host: host.into(),
            deadline,
            reply: sender,
        })
        .map_err(|_| io::Error::other("push resolver busy"))?;
    let addresses = receiver
        .recv_timeout(remaining(deadline)?)
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "push DNS deadline elapsed"))??;
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses
            .iter()
            .any(|address| !public_unicast(address.ip()))
    {
        return Err(io::Error::other(
            "push DNS must resolve exclusively to bounded public unicast addresses",
        ));
    }
    Ok(addresses)
}

fn public_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Only ordinary global unicast; exclude mapped/translation, Teredo,
            // benchmarking, ORCHID, documentation and 6to4 ranges.
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
        }
    }
}

enum Stream {
    Https(SslStream<TcpStream>),
    #[cfg(test)]
    Fixture(TcpStream),
}
impl Stream {
    fn socket(&self) -> &TcpStream {
        match self {
            Self::Https(stream) => stream.get_ref(),
            #[cfg(test)]
            Self::Fixture(stream) => stream,
        }
    }
    fn read_bounded(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        loop {
            remaining(deadline)?;
            let result = match self {
                Self::Https(stream) => stream.ssl_read(bytes).map_err(|error| match error.code() {
                    ErrorCode::WANT_READ => Some(libc::POLLIN),
                    ErrorCode::WANT_WRITE => Some(libc::POLLOUT),
                    _ => None,
                }),
                #[cfg(test)]
                Self::Fixture(stream) => stream.read(bytes).map_err(|error| {
                    (error.kind() == io::ErrorKind::WouldBlock).then_some(libc::POLLIN)
                }),
            };
            match result {
                Ok(count) => return Ok(count),
                Err(Some(events)) => wait_ready(self.socket(), events, deadline)?,
                Err(None) => return Err(io::Error::other("push response unavailable")),
            }
        }
    }
    fn write_bounded(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<usize> {
        loop {
            remaining(deadline)?;
            let result = match self {
                Self::Https(stream) => {
                    stream.ssl_write(bytes).map_err(|error| match error.code() {
                        ErrorCode::WANT_READ => Some(libc::POLLIN),
                        ErrorCode::WANT_WRITE => Some(libc::POLLOUT),
                        _ => None,
                    })
                }
                #[cfg(test)]
                Self::Fixture(stream) => stream.write(bytes).map_err(|error| {
                    (error.kind() == io::ErrorKind::WouldBlock).then_some(libc::POLLOUT)
                }),
            };
            match result {
                Ok(count) => return Ok(count),
                Err(Some(events)) => wait_ready(self.socket(), events, deadline)?,
                Err(None) => return Err(io::Error::other("push request unavailable")),
            }
        }
    }
}

fn wait_ready(socket: &TcpStream, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let timeout = remaining(deadline)?.as_millis().saturating_add(1).min(3000) as libc::c_int;
        let mut descriptor = libc::pollfd {
            fd: socket.as_raw_fd(),
            events,
            revents: 0,
        };
        // The descriptor remains owned by this session for the complete call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result > 0 {
            remaining(deadline)?;
            return Ok(());
        }
        if result == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
impl PushSender {
    fn connect(&self, host: &str, deadline: Instant) -> io::Result<Stream> {
        #[cfg(test)]
        if let Some((address, false)) = self.fixture {
            let socket = TcpStream::connect_timeout(&address, remaining(deadline)?)?;
            socket.set_nonblocking(true)?;
            return Ok(Stream::Fixture(socket));
        }
        #[cfg(test)]
        let addresses = if let Some((address, true)) = self.fixture {
            vec![address]
        } else {
            resolve(host, deadline)?
        };
        #[cfg(not(test))]
        let addresses = resolve(host, deadline)?;
        let mut connected = None;
        for address in addresses {
            if let Ok(stream) = TcpStream::connect_timeout(&address, remaining(deadline)?) {
                connected = Some(stream);
                break;
            }
        }
        let socket = connected.ok_or_else(|| io::Error::other("push connection unavailable"))?;
        socket.set_nonblocking(true)?;
        // Connect the already validated address while retaining SNI and certificate
        // verification against the original provider hostname. No second DNS lookup.
        let mut handshake = self.tls.connect(host, socket);
        loop {
            match handshake {
                Ok(stream) => return Ok(Stream::Https(stream)),
                Err(HandshakeError::WouldBlock(stream)) => {
                    let events = match stream.error().code() {
                        ErrorCode::WANT_READ => libc::POLLIN,
                        ErrorCode::WANT_WRITE => libc::POLLOUT,
                        _ => return Err(io::Error::other("push TLS connection unavailable")),
                    };
                    wait_ready(stream.get_ref(), events, deadline)?;
                    handshake = stream.handshake();
                }
                Err(_) => return Err(io::Error::other("push TLS connection unavailable")),
            }
        }
    }
}

fn write_request(stream: &mut Stream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let written = stream.write_bounded(bytes, deadline)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

struct Response {
    code: u16,
    ttl: Option<u32>,
}

fn read_response(stream: &mut Stream, deadline: Instant) -> io::Result<Response> {
    let mut headers = Vec::new();
    while headers.len() < MAX_RESPONSE_HEADERS {
        let mut byte = [0];
        if stream.read_bounded(&mut byte, deadline)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    if !headers.ends_with(b"\r\n\r\n") {
        return Err(io::Error::other("push response headers exceeded limit"));
    }
    let headers = std::str::from_utf8(&headers)
        .map_err(|_| io::Error::other("push response headers invalid"))?;
    let mut lines = headers.split("\r\n");
    let mut status = lines.next().unwrap_or_default().splitn(3, ' ');
    if !matches!(status.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err(io::Error::other("push response protocol invalid"));
    }
    let code = status
        .next()
        .filter(|code| code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (200..600).contains(code))
        .ok_or_else(|| io::Error::other("push response status invalid"))?;
    let mut ttl = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("push response header invalid"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(io::Error::other("push response header invalid"));
        }
        if name.eq_ignore_ascii_case("ttl") {
            if ttl.is_some() {
                return Err(io::Error::other("push response TTL repeated"));
            }
            ttl = Some(
                value
                    .trim()
                    .parse()
                    .map_err(|_| io::Error::other("push response TTL invalid"))?,
            );
        }
    }
    // Provider bodies convey no trusted delivery evidence. Read zero body bytes,
    // then close the connection: consumption stays below the 4096-byte bound and
    // raw provider text, endpoint tokens and credentials cannot enter history.
    Ok(Response { code, ttl })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::{Aes128Gcm, KeyInit, Nonce, aead::Aead};
    use hkdf::Hkdf;
    use openssl::{
        bn::BigNum,
        derive::Deriver,
        ec::{EcKey, PointConversionForm},
        ecdsa::EcdsaSig,
        hash::MessageDigest,
        pkey::{PKey, Private},
        sign::Verifier,
    };
    use std::net::TcpListener;

    const NOW: i64 = 1_800_000_000;
    const ENDPOINT: &str = "https://fcm.googleapis.com/fcm/send/private-fixture-token";

    fn config() -> Arc<PushConfig> {
        Arc::new(PushConfig {
            vapid_private_key: URL_SAFE_NO_PAD.encode([7; 32]),
            subject: "https://bokkie.example.org".into(),
            timeout_ms: 1000,
            ttl_seconds: 3600,
        })
    }

    fn intent() -> (NotificationIntent, PKey<Private>) {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let key = EcKey::generate(&group).unwrap();
        let public = key
            .public_key()
            .to_bytes(
                &group,
                PointConversionForm::UNCOMPRESSED,
                &mut BigNumContext::new().unwrap(),
            )
            .unwrap();
        let intent = NotificationIntent {
            id: "delivery-11111111-1111-4111-8111-111111111111".into(),
            task_id: "task-22222222-2222-4222-8222-222222222222".into(),
            source_obligation_id: "source-fixture".into(),
            destination: "Bokkie device".into(),
            subject: "Remember the café".into(),
            body: "Take the notes.".into(),
            message_id: "fixture-message".into(),
            created_at: NOW,
            transport: None,
            push: Some(PushIntent {
                subscription: PushSubscription {
                    id: "33333333-3333-4333-8333-333333333333".into(),
                    label: "Test device".into(),
                    endpoint: ENDPOINT.into(),
                    p256dh: URL_SAFE_NO_PAD.encode(public),
                    auth: URL_SAFE_NO_PAD.encode([9; 16]),
                    vapid_public_key: config().public_key().unwrap(),
                },
                receipt_token: "0b".repeat(32),
                expires_at: NOW + 3600,
            }),
        };
        (intent, PKey::from_ec_key(key).unwrap())
    }

    fn sender() -> PushSender {
        PushSender::new(config(), Arc::new(crate::ManualClock::new(NOW))).unwrap()
    }

    fn request_parts(bytes: &[u8]) -> (&str, &[u8]) {
        let start = bytes
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        (
            std::str::from_utf8(&bytes[..start]).unwrap(),
            &bytes[start..],
        )
    }
    fn header<'a>(headers: &'a str, name: &str) -> &'a str {
        headers
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .unwrap()
            .1
            .trim()
    }
    fn read_request(stream: &mut impl Read) -> Vec<u8> {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < 8192);
        }
        let body_len: usize = header(std::str::from_utf8(&bytes).unwrap(), "Content-Length")
            .parse()
            .unwrap();
        let offset = bytes.len();
        bytes.resize(offset + body_len, 0);
        stream.read_exact(&mut bytes[offset..]).unwrap();
        bytes
    }

    fn exchange(reply: &[u8]) -> (NotificationOutcome, Vec<u8>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let check = listener.try_clone().unwrap();
        let mut sender = sender();
        sender.fixture = Some((listener.local_addr().unwrap(), false));
        let reply = reply.to_vec();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            if !reply.is_empty() {
                let _ = stream.write_all(&reply);
            }
            request
        });
        let outcome = sender.send(&intent().0);
        let bytes = peer.join().unwrap();
        check.set_nonblocking(true).unwrap();
        assert_eq!(
            check.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "an attempt made more than one POST"
        );
        (outcome, bytes)
    }

    // Independent RFC 8291 key schedule and RustCrypto GCM decryption. No use of
    // ece's decrypt function, so encryption and decryption do not share code.
    fn decrypt(
        bytes: &[u8],
        receiver: &PKey<Private>,
        auth: &[u8],
        receiver_public: &[u8],
    ) -> Vec<u8> {
        assert_eq!(bytes[20], 65);
        let sender_public = &bytes[21..86];
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let point =
            EcPoint::from_bytes(&group, sender_public, &mut BigNumContext::new().unwrap()).unwrap();
        let public = PKey::from_ec_key(EcKey::from_public_key(&group, &point).unwrap()).unwrap();
        let mut derive = Deriver::new(receiver).unwrap();
        derive.set_peer(&public).unwrap();
        let shared = derive.derive_to_vec().unwrap();
        let mut info = b"WebPush: info\0".to_vec();
        info.extend(receiver_public);
        info.extend(sender_public);
        let mut ikm = [0; 32];
        Hkdf::<Sha256>::new(Some(auth), &shared)
            .expand(&info, &mut ikm)
            .unwrap();
        let hkdf = Hkdf::<Sha256>::new(Some(&bytes[..16]), &ikm);
        let mut key = [0; 16];
        hkdf.expand(b"Content-Encoding: aes128gcm\0", &mut key)
            .unwrap();
        let mut nonce = [0; 12];
        hkdf.expand(b"Content-Encoding: nonce\0", &mut nonce)
            .unwrap();
        let mut decoded = Aes128Gcm::new_from_slice(&key)
            .unwrap()
            .decrypt(Nonce::from_slice(&nonce), &bytes[86..])
            .unwrap();
        let end = decoded.iter().rposition(|byte| *byte != 0).unwrap();
        assert_eq!(decoded[end], 2);
        decoded.truncate(end);
        decoded
    }

    #[test]
    fn encrypted_payload_and_vapid_are_independently_verified() {
        let (intent, receiver) = intent();
        let request = sender().request(&intent, NOW).unwrap();
        let (headers, body) = request_parts(&request.bytes);
        let subscription = &intent.push.as_ref().unwrap().subscription;
        let decoded = decrypt(
            body,
            &receiver,
            &URL_SAFE_NO_PAD.decode(&subscription.auth).unwrap(),
            &URL_SAFE_NO_PAD.decode(&subscription.p256dh).unwrap(),
        );
        assert_eq!(decoded, payload(&intent).unwrap());
        let json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["task_id"], intent.task_id);
        assert!(json.get("endpoint").is_none());
        assert!(json.get("url").is_none());
        assert!(
            !body
                .windows(intent.body.len())
                .any(|bytes| bytes == intent.body.as_bytes())
        );
        assert_eq!(header(headers, "Content-Encoding"), "aes128gcm");
        let (token, key) = header(headers, "Authorization")
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(key, subscription.vapid_public_key);
        let segments: Vec<_> = token.split('.').collect();
        assert_eq!(segments.len(), 3);
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segments[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], config().subject);
        assert_eq!(claims["exp"], NOW + 43200);
        let public = URL_SAFE_NO_PAD.decode(key).unwrap();
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let point =
            EcPoint::from_bytes(&group, &public, &mut BigNumContext::new().unwrap()).unwrap();
        let pkey = PKey::from_ec_key(EcKey::from_public_key(&group, &point).unwrap()).unwrap();
        let sig = URL_SAFE_NO_PAD.decode(segments[2]).unwrap();
        assert_eq!(sig.len(), 64);
        let der = EcdsaSig::from_private_components(
            BigNum::from_slice(&sig[..32]).unwrap(),
            BigNum::from_slice(&sig[32..]).unwrap(),
        )
        .unwrap()
        .to_der()
        .unwrap();
        let mut verifier = Verifier::new(MessageDigest::sha256(), &pkey).unwrap();
        verifier
            .update(format!("{}.{}", segments[0], segments[1]).as_bytes())
            .unwrap();
        assert!(verifier.verify(&der).unwrap());
    }

    #[test]
    fn subscription_policy_rejects_endpoint_and_key_bypasses() {
        let (intent, _) = intent();
        let sub = &intent.push.unwrap().subscription;
        for endpoint in [
            ENDPOINT,
            "https://api.push.apple.com/3/device/token",
            "https://updates.push.services.mozilla.com/wpush/v2/token",
            "https://fcm.googleapis.com:443/fcm/send/token",
        ] {
            validate_subscription(endpoint, &sub.p256dh, &sub.auth).unwrap();
        }
        for endpoint in [
            "http://fcm.googleapis.com/fcm/send/token",
            "https://127.0.0.1/token",
            "https://[::1]/token",
            "https://fcm.googleapis.com:8443/token",
            "https://user@fcm.googleapis.com/token",
            "https://@fcm.googleapis.com/token",
            "https://fcm.googleapis.com/token?x=1",
            "https://fcm.googleapis.com/token#fragment",
            "https://fcm.googleapis.com/",
            "https://fcm.googleapis.com.evil.example/token",
            "https://push.apple.com/token",
            "https://evilpush.apple.com/token",
            "https://fcm.googleapis.com./token",
            "https://fcm.googleapis.com\\@localhost/token",
            "https://fcm.googleapis.com/token\r\nHost: localhost",
        ] {
            assert!(
                validate_subscription(endpoint, &sub.p256dh, &sub.auth).is_err(),
                "endpoint policy accepted a prohibited case"
            );
        }
        assert!(
            validate_subscription(
                &format!("{ENDPOINT}{}", "a".repeat(2048)),
                &sub.p256dh,
                &sub.auth
            )
            .is_err()
        );
        let mut off_curve = vec![0; 65];
        off_curve[0] = 4;
        assert!(
            validate_subscription(ENDPOINT, &URL_SAFE_NO_PAD.encode(off_curve), &sub.auth).is_err()
        );
        assert!(
            validate_subscription(ENDPOINT, &URL_SAFE_NO_PAD.encode([3; 65]), &sub.auth).is_err()
        );
        assert!(
            validate_subscription(ENDPOINT, &sub.p256dh, &URL_SAFE_NO_PAD.encode([1; 15])).is_err()
        );
        assert!(validate_subscription(ENDPOINT, &format!("{}=", sub.p256dh), &sub.auth).is_err());
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:0808:0808::1",
            "3fff::1",
        ] {
            assert!(
                !public_unicast(ip.parse().unwrap()),
                "reserved address accepted: {ip}"
            );
        }
        for ip in ["8.8.8.8", "142.250.70.202", "2606:4700:4700::1111"] {
            assert!(public_unicast(ip.parse().unwrap()));
        }
    }

    #[test]
    fn configuration_is_strict_bounded_and_redacted() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("push.json");
        let config = config();
        let raw = serde_json::json!({"vapid_private_key": config.vapid_private_key,"subject":config.subject,"timeout_ms":1000,"ttl_seconds":3600}).to_string();
        std::fs::write(&path, &raw).unwrap();
        PushConfig::load(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        assert!(PushConfig::load(Path::new("relative.json")).is_err());
        assert!(PushConfig::load(temp.path()).is_err());
        std::fs::write(&path, raw.replace("1000", "1000,\"unknown\":1")).unwrap();
        assert!(PushConfig::load(&path).is_err());
        std::fs::write(&path, " ".repeat(8193)).unwrap();
        assert!(PushConfig::load(&path).is_err());
        for subject in [
            "mailto:someone@example.org",
            "https://localhost",
            "https://127.0.0.1",
            "https://example.local",
            "https://example.org/contact",
            "https://user@example.org",
        ] {
            let mut invalid = (*config).clone();
            invalid.subject = subject.into();
            assert!(invalid.validate().is_err());
        }
        for timeout_ms in [0, 99, 3001, u64::MAX] {
            let mut invalid = (*config).clone();
            invalid.timeout_ms = timeout_ms;
            assert!(invalid.validate().is_err());
        }
        for ttl_seconds in [0, 59, 86401] {
            let mut invalid = (*config).clone();
            invalid.ttl_seconds = ttl_seconds;
            assert!(invalid.validate().is_err());
        }
        let mut invalid = (*config).clone();
        invalid.vapid_private_key = URL_SAFE_NO_PAD.encode([0; 32]);
        assert!(invalid.validate().is_err());
        let (intent, _) = intent();
        let debug = format!("{config:?} {:?}", intent.push);
        for secret in [
            &config.vapid_private_key,
            &intent.push.as_ref().unwrap().subscription.endpoint,
            &intent.push.as_ref().unwrap().subscription.auth,
            &intent.push.as_ref().unwrap().receipt_token,
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn payload_bounds_topic_identity_and_lifetime_are_stable() {
        let (mut intent, _) = intent();
        intent.body.clear();
        let overhead = payload(&intent).unwrap().len();
        intent.body = "a".repeat(3200 - overhead);
        assert_eq!(payload(&intent).unwrap().len(), 3200);
        let sender = sender();
        let first = sender.request(&intent, NOW).unwrap();
        assert!(request_parts(&first.bytes).1.len() <= 4096);
        let later = sender.request(&intent, NOW + 120).unwrap();
        assert_eq!(first.ttl, 3600);
        assert_eq!(later.ttl, 3480);
        let topic = header(request_parts(&first.bytes).0, "Topic");
        assert_eq!(topic.len(), 32);
        assert_eq!(topic, header(request_parts(&later.bytes).0, "Topic"));
        assert!(sender.request(&intent, NOW + 3600).is_err());
        intent.body.push('a');
        assert!(payload(&intent).is_err());
        intent.body.clear();
        for id in [
            "11111111-1111-4111-8111-111111111111",
            "task-11111111-1111-4111-8111-111111111111",
            "delivery-11111111111141118111111111111111",
            "delivery-11111111-1111-4111-8111-111111111111/other",
        ] {
            let mut invalid = intent.clone();
            invalid.id = id.into();
            assert!(payload(&invalid).is_err());
        }
        for task_id in [
            "22222222-2222-4222-8222-222222222222",
            "delivery-22222222-2222-4222-8222-222222222222",
            "task-22222222-2222-4222-8222-222222222222?other",
        ] {
            let mut invalid = intent.clone();
            invalid.task_id = task_id.into();
            assert!(payload(&invalid).is_err());
        }
        for token in [
            "0b".repeat(31),
            "AB".repeat(32),
            "gg".repeat(32),
            URL_SAFE_NO_PAD.encode([11; 32]),
        ] {
            let mut invalid = intent.clone();
            invalid.push.as_mut().unwrap().receipt_token = token;
            assert!(payload(&invalid).is_err());
        }
        let id = "33333333-3333-4333-8333-333333333333";
        assert_eq!(push_device_id(&push_profile_id(id)), Some(id));
        for profile in [
            "reminder-web-push-v1/no-id",
            "reminder-web-push-v1/33333333333343338333333333333333",
            "reminder-web-push-v1/33333333-3333-4333-8333-333333333333/other",
            "reminder-v1",
        ] {
            assert!(push_device_id(profile).is_none());
        }
    }

    #[test]
    fn store_created_intent_reaches_payload_and_encryption_with_its_exact_identity() {
        use bokkie_operator_api::{
            ManagedTaskDefinition, PushKeys, PushRegisterRequest, ServiceIdentity,
        };
        let (fixture, receiver) = intent();
        let subscription = &fixture.push.as_ref().unwrap().subscription;
        let config = config();
        let key = config.public_key().unwrap();
        let mut store = crate::Store::open_in_memory().unwrap();
        let service = ServiceIdentity {
            build: "fixture".into(),
            api_contract_version: 1,
            schema_version: 15,
            process_id: 1,
            session_id: "push-test-session".into(),
        };
        let revision = store
            .push_setup(service.clone(), Some(key.clone()), config.ttl_seconds)
            .unwrap()
            .configuration_revision;
        store
            .register_push(
                &PushRegisterRequest {
                    command_id: uuid::Uuid::new_v4().to_string(),
                    configuration_revision: revision,
                    label: "Transport fixture".into(),
                    endpoint: subscription.endpoint.clone(),
                    keys: PushKeys {
                        p256dh: subscription.p256dh.clone(),
                        auth: subscription.auth.clone(),
                    },
                },
                service,
                &key,
                config.ttl_seconds,
                NOW,
            )
            .unwrap();
        let profile = store.push_profile(&key).unwrap().unwrap();
        let mut definition = ManagedTaskDefinition::reminder(
            "Review notes",
            "Read the notes.",
            &profile.destination,
        );
        definition.profile_revision = profile.revision.clone();
        definition.max_output_chars = 2000;
        let task = store
            .managed_create(&uuid::Uuid::new_v4().to_string(), &definition, NOW)
            .unwrap()
            .task_id;
        let review = store
            .managed_preview(
                &task,
                "push-test-session",
                std::slice::from_ref(&profile),
                NOW,
            )
            .unwrap();
        assert!(review.blockers.is_empty(), "{:?}", review.blockers);
        store
            .managed_activate(
                &uuid::Uuid::new_v4().to_string(),
                &review,
                "push-test-session",
                &[profile],
                NOW,
            )
            .unwrap();
        assert!(crate::managed::run_one_reminder(&mut store, NOW).unwrap());
        let delivery = store
            .managed_detail(&task)
            .unwrap()
            .runs
            .remove(0)
            .delivery
            .unwrap();
        let saved = store.notification_intent(&delivery.id).unwrap();
        assert!(saved.id.starts_with("delivery-"));
        assert!(saved.task_id.starts_with("task-"));
        let token = &saved.push.as_ref().unwrap().receipt_token;
        assert_eq!(token.len(), 64);
        let payload = payload(&saved).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(json["id"], delivery.id);
        assert_eq!(json["task_id"], task);
        assert_eq!(json["receipt_token"], *token);
        let request = sender().request(&saved, NOW).unwrap();
        assert_eq!(
            decrypt(
                request_parts(&request.bytes).1,
                &receiver,
                &URL_SAFE_NO_PAD.decode(&subscription.auth).unwrap(),
                &URL_SAFE_NO_PAD.decode(&subscription.p256dh).unwrap()
            ),
            payload
        );
    }

    #[test]
    fn accepted_expired_rejected_and_redirect_outcomes_are_distinct() {
        let (accepted, _) = exchange(b"HTTP/1.1 201 Created\r\nTTL: 99999\r\nContent-Length: 1000000000\r\n\r\nprivate provider body is ignored");
        assert!(matches!(
            accepted,
            NotificationOutcome::PushAccepted {
                ttl_seconds: 3600,
                ..
            }
        ));
        let (shorter, _) = exchange(b"HTTP/1.1 201 Created\r\nTTL: 30\r\n\r\n");
        assert!(matches!(
            shorter,
            NotificationOutcome::PushAccepted {
                ttl_seconds: 30,
                ..
            }
        ));
        for code in [404, 410] {
            assert!(matches!(
                exchange(format!("HTTP/1.1 {code} Expired\r\n\r\n").as_bytes()).0,
                NotificationOutcome::SubscriptionExpired { .. }
            ));
        }
        for code in [429, 500, 503] {
            assert!(matches!(
                exchange(format!("HTTP/1.1 {code} Reject\r\n\r\n").as_bytes()).0,
                NotificationOutcome::Rejected {
                    retryable: true,
                    ..
                }
            ));
        }
        for code in [200, 202, 400, 401, 403, 413] {
            assert!(matches!(
                exchange(format!("HTTP/1.1 {code} Reject\r\n\r\n").as_bytes()).0,
                NotificationOutcome::Rejected {
                    retryable: false,
                    ..
                }
            ));
        }
        let (redirect, _) = exchange(
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1/forbidden\r\n\r\n",
        );
        assert!(matches!(
            redirect,
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn lost_response_malformed_reply_and_total_deadline_require_reconciliation() {
        assert!(matches!(
            exchange(b"").0,
            NotificationOutcome::Uncertain { .. }
        ));
        for reply in [
            b"HTTP/1.1 201 Created\r\nTTL: invalid\r\n\r\n".as_slice(),
            b"HTTP/1.1 201 Created\r\nTTL: 1\r\nTTL: 2\r\n\r\n",
            b"HTTP/1.1 201 Created\r\ninvalid\r\n\r\n",
        ] {
            assert!(matches!(
                exchange(reply).0,
                NotificationOutcome::Uncertain { .. }
            ));
        }
        let oversized = format!("HTTP/1.1 201 Created\r\nX: {}\r\n\r\n", "a".repeat(8192));
        assert!(matches!(
            exchange(oversized.as_bytes()).0,
            NotificationOutcome::Uncertain { .. }
        ));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = sender();
        let mut config = (*config()).clone();
        config.timeout_ms = 100;
        sender.config = Arc::new(config);
        sender.fixture = Some((listener.local_addr().unwrap(), false));
        let (release, wait) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        let start = Instant::now();
        assert!(matches!(
            sender.send(&intent().0),
            NotificationOutcome::Uncertain { .. }
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn key_mismatch_and_expiry_fail_before_any_post_and_do_not_reroute() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut sender = sender();
        sender.fixture = Some((listener.local_addr().unwrap(), false));
        let (mut intent, _) = intent();
        intent.push.as_mut().unwrap().subscription.vapid_public_key =
            "different reviewed key".into();
        assert!(matches!(
            sender.send(&intent),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        intent.push.as_mut().unwrap().subscription.vapid_public_key =
            config().public_key().unwrap();
        intent.push.as_mut().unwrap().expires_at = NOW;
        assert!(matches!(
            sender.send(&intent),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(sender.transport().is_none());
        assert_eq!(sender.reminder_modes(), (false, true));
    }

    fn https_fixture(
        certificate_host: &str,
    ) -> (PushSender, TcpListener, openssl::ssl::SslAcceptor) {
        use openssl::{
            asn1::Asn1Time,
            rsa::Rsa,
            x509::{X509, X509NameBuilder, extension::SubjectAlternativeName},
        };
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", certificate_host).unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap())
            .unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        let san = SubjectAlternativeName::new()
            .dns(certificate_host)
            .build(&cert.x509v3_context(None, None))
            .unwrap();
        cert.append_extension(san).unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let cert = cert.build();
        let mut acceptor =
            openssl::ssl::SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
        acceptor.set_private_key(&key).unwrap();
        acceptor.set_certificate(&cert).unwrap();
        let mut sender = sender();
        let mut connector = SslConnector::builder(SslMethod::tls_client()).unwrap();
        connector.cert_store_mut().add_cert(cert).unwrap();
        sender.tls = connector.build();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        sender.fixture = Some((listener.local_addr().unwrap(), true));
        (sender, listener, acceptor.build())
    }

    #[test]
    fn https_uses_original_hostname_and_rejects_a_different_certificate() {
        for (certificate_host, accepted) in [
            ("fcm.googleapis.com", true),
            ("different.example.org", false),
        ] {
            let (sender, listener, acceptor) = https_fixture(certificate_host);
            let peer = thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                match acceptor.accept(socket) {
                    Ok(mut stream) => {
                        let request = read_request(&mut stream);
                        stream
                            .write_all(b"HTTP/1.1 201 Created\r\nTTL: 60\r\n\r\n")
                            .unwrap();
                        Some(request)
                    }
                    Err(_) => None,
                }
            });
            let outcome = sender.send(&intent().0);
            if accepted {
                assert!(matches!(
                    outcome,
                    NotificationOutcome::PushAccepted {
                        ttl_seconds: 60,
                        ..
                    }
                ));
                let request = peer.join().unwrap().unwrap();
                assert_eq!(
                    header(request_parts(&request).0, "Host"),
                    "fcm.googleapis.com"
                );
            } else {
                assert!(matches!(
                    outcome,
                    NotificationOutcome::Rejected {
                        retryable: true,
                        ..
                    }
                ));
                assert!(peer.join().unwrap().is_none());
            }
        }
    }

    #[test]
    fn tls_handshake_is_inside_total_deadline_before_post() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = sender();
        let mut config = (*config()).clone();
        config.timeout_ms = 100;
        sender.config = Arc::new(config);
        sender.fixture = Some((listener.local_addr().unwrap(), true));
        let (release, wait) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut client_hello = [0; 4096];
            let bytes = stream.read(&mut client_hello).unwrap();
            assert!(bytes > 0);
            assert_ne!(&client_hello[..bytes.min(4)], b"POST");
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        let start = Instant::now();
        assert!(matches!(
            sender.send(&intent().0),
            NotificationOutcome::Rejected {
                retryable: true,
                ..
            }
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        peer.join().unwrap();
    }
}
