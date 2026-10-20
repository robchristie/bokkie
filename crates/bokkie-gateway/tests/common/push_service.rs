//! A mock Web Push service, and the devices subscribed to it, for push
//! tests.
//!
//! The service answers over HTTPS on loopback for a few made-up names
//! under `.example`, with a certificate from its own authority, as
//! [`super::site`] does. Each device holds its own P-256 key and secret, as
//! a browser does, so a test reads a push only by decrypting it with the
//! keys of the device it was meant for. The service checks each request's
//! VAPID token and headers as a push service would, records what it
//! received, and answers each device's next pushes as a test scripts.
//! The [`TestNetwork`] resolves only these names, so no test reaches a real
//! push service or the internet.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bokkie_gateway::http::test_network::TestNetwork;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls;

/// Names the service's certificate holds. Both resolve to it.
pub const NAMES: &[&str] = &["push.example", "updates.push.example"];

/// One push the service received, checked.
#[derive(Debug, Clone)]
pub struct Received {
    pub host: String,
    pub path: String,
    pub headers: HeaderMap,
    /// The encrypted body.
    pub body: Vec<u8>,
    /// The VAPID token's claims, if its signature verified against the key
    /// the request named.
    pub claims: Option<Value>,
    /// The public key the request named, base64url.
    pub key: Option<String>,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// How the service answers one push.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    pub fn status(status: u16) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap(),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// Apple's answer when the subscription was made with another key.
    pub fn key_mismatch() -> Self {
        Self {
            body: json!({"reason": "VapidPkHashMismatch"}).to_string(),
            ..Self::status(403)
        }
    }

    pub fn retry_after(mut self, seconds: u32) -> Self {
        self.headers
            .push(("retry-after".into(), seconds.to_string()));
        self
    }
}

#[derive(Clone)]
pub struct PushService {
    pub address: SocketAddr,
    ca_der: Vec<u8>,
    received: Arc<Mutex<Vec<Received>>>,
    script: Arc<Mutex<HashMap<String, VecDeque<Answer>>>>,
}

/// A browser's subscription on one device: its endpoint and the keys only
/// it holds.
pub struct Device {
    pub endpoint: String,
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Device {
    /// A device subscribed at `https://push.example/<path>`.
    pub fn new(path: &str) -> Self {
        Self::at(&format!("https://push.example/{path}"))
    }

    /// A device subscribed at `endpoint`.
    pub fn at(endpoint: &str) -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).unwrap();
        let mut auth = [0u8; 16];
        getrandom::fill(&mut auth).unwrap();
        Self {
            endpoint: endpoint.to_owned(),
            secret: p256::SecretKey::from_slice(&bytes).unwrap(),
            auth,
        }
    }

    /// The subscription as the browser's `toJSON` gives it.
    pub fn subscription(&self) -> Value {
        let point = self.secret.public_key().to_encoded_point(false);
        json!({
            "endpoint": self.endpoint,
            "expirationTime": null,
            "keys": {
                "p256dh": URL_SAFE_NO_PAD.encode(point.as_bytes()),
                "auth": URL_SAFE_NO_PAD.encode(self.auth),
            },
        })
    }

    pub fn path(&self) -> String {
        url::Url::parse(&self.endpoint).unwrap().path().to_owned()
    }

    /// Decrypts a push meant for this device, as its browser would, and
    /// reads its declarative message. `None` if this device's keys do not
    /// open it.
    pub fn open(&self, push: &Received) -> Option<Value> {
        let auth = web_push_native::Auth::clone_from_slice(&self.auth);
        let plain = web_push_native::decrypt(push.body.clone(), &self.secret, &auth).ok()?;
        Some(serde_json::from_slice(&plain).expect("a push is JSON"))
    }
}

async fn receive(State(service): State<PushService>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap().to_vec();
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let path = parts.uri.path().to_owned();
    let (claims, key) = match vapid(&parts.headers) {
        Some((claims, key)) => (claims, Some(key)),
        None => (None, None),
    };
    let checked = parts.method == axum::http::Method::POST && claims.is_some();
    service.received.lock().unwrap().push(Received {
        host,
        path: path.clone(),
        headers: parts.headers,
        body,
        claims,
        key,
    });
    if !checked {
        return (StatusCode::UNAUTHORIZED, "bad request").into_response();
    }
    let answer = service
        .script
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front)
        .unwrap_or_else(|| Answer::status(201));
    let mut response = (answer.status, answer.body).into_response();
    for (name, value) in answer.headers {
        response.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    response
}

/// Reads `Authorization: vapid t=<jwt>, k=<key>` and verifies the token's
/// ES256 signature with the key it names. Returns the claims if it
/// verified, and the key.
fn vapid(headers: &HeaderMap) -> Option<(Option<Value>, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = value.strip_prefix("vapid ")?;
    let mut token = None;
    let mut key = None;
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(t) = part.strip_prefix("t=") {
            token = Some(t);
        } else if let Some(k) = part.strip_prefix("k=") {
            key = Some(k);
        }
    }
    let (token, key) = (token?, key?);
    let verify = || -> Option<Value> {
        let public = URL_SAFE_NO_PAD.decode(key).ok()?;
        let verifying = VerifyingKey::from_sec1_bytes(&public).ok()?;
        let (input, signature) = token.rsplit_once('.')?;
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
        verifying.verify(input.as_bytes(), &signature).ok()?;
        let (header, claims) = input.split_once('.')?;
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
        if header["alg"] != "ES256" {
            return None;
        }
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).ok()?).ok()
    };
    Some((verify(), key.to_owned()))
}

impl PushService {
    pub async fn start() -> Self {
        let (ca_der, tls) = tls_config();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = Self {
            address: listener.local_addr().unwrap(),
            ca_der,
            received: Default::default(),
            script: Default::default(),
        };
        let app = Router::new().fallback(receive).with_state(service.clone());
        let listener = TlsListener {
            listener,
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(tls)),
        };
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        service
    }

    /// A network that reaches this service, and nothing else: each of
    /// [`NAMES`] resolves to loopback, port 443 to the service's port.
    pub fn network(&self) -> Arc<TestNetwork> {
        let network = TestNetwork::new()
            .allow_loopback()
            .map_port(443, self.address.port())
            .trust(self.ca_der.clone());
        for name in NAMES {
            network.answer(name, &["127.0.0.1"]);
        }
        Arc::new(network)
    }

    /// Answers the device's next pushes with `answers`, in order, then 201.
    pub fn script(&self, device: &Device, answers: impl IntoIterator<Item = Answer>) {
        self.script
            .lock()
            .unwrap()
            .entry(device.path())
            .or_default()
            .extend(answers);
    }

    /// Every push received so far.
    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    /// The pushes received for `device`'s endpoint.
    pub fn sent_to(&self, device: &Device) -> Vec<Received> {
        let path = device.path();
        self.received()
            .into_iter()
            .filter(|r| r.path == path)
            .collect()
    }

    /// The messages `device` opened from the pushes sent to its endpoint.
    /// Panics if one does not open with its keys.
    pub fn opened(&self, device: &Device) -> Vec<Value> {
        self.sent_to(device)
            .iter()
            .map(|push| device.open(push).expect("the device's keys open its push"))
            .collect()
    }

    pub fn clear(&self) {
        self.received.lock().unwrap().clear();
    }
}

/// Accepts TLS connections for `axum::serve`.
struct TlsListener {
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, address)) = self.listener.accept().await else {
                continue;
            };
            let handshake = self.acceptor.accept(stream);
            if let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(5), handshake).await {
                return (stream, address);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// A certificate authority made for this service alone, and a server
/// configuration for [`NAMES`] signed by it. Returns the authority in DER.
fn tls_config() -> (Vec<u8>, rustls::ServerConfig) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Bokkie test push authority");
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let names: Vec<String> = NAMES.iter().map(|n| (*n).to_owned()).collect();
    let leaf = CertificateParams::new(names)
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone(), ca_cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        )
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    (ca_cert.der().to_vec(), config)
}
