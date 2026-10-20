//! The gateway's VAPID key (RFC 8292) and the tokens it signs.
//!
//! The key is a P-256 private key the gateway generates on first start into
//! `<data>/push/vapid.pem` (PKCS#8 PEM, mode 600, in a directory of mode
//! 700). It never leaves that file: only its public half is served, and
//! nothing logs it. Every subscription is made for the public key, so
//! losing or replacing the file ends every subscription. A file that is
//! there but unreadable, readable by others or not a P-256 key stops the
//! gateway from starting; it is never replaced silently.
//!
//! A push service checks each request's `Authorization: vapid t=<token>,
//! k=<public key>`, where the token is an ES256 JSON Web Token naming the
//! push service's origin (`aud`), an expiry (`exp`) and the operator's
//! contact (`sub`). Tokens are cached per push service and reused for
//! [`TOKEN_REUSE_MS`]. A token the push service refused (Apple's `403`
//! `BadJwtToken`) is replaced once it is [`TOKEN_RENEWAL_MS`] old, so the
//! gateway never makes a new one for a service more than once an hour.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bokkie_protocol::Timestamp;
use p256::SecretKey;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::zeroize::Zeroizing;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};

use crate::clock::HOUR_MS;

/// The key's file, in the push directory.
pub const FILE: &str = "vapid.pem";
/// How far ahead a token expires: Apple accepts at most a day.
pub const TOKEN_LIFETIME_MS: i64 = 12 * HOUR_MS;
/// How long a token is reused before a new one is made.
pub const TOKEN_REUSE_MS: i64 = 6 * HOUR_MS;
/// How long a token a push service refused is still reused before a new
/// one is made: Apple asks for at most one new token an hour.
pub const TOKEN_RENEWAL_MS: i64 = HOUR_MS;
/// How many push services' tokens are cached at once.
const CACHED_TOKENS: usize = 64;

/// The gateway's VAPID key.
pub struct Key {
    signing: SigningKey,
    /// The public key, uncompressed, in unpadded base64url: what a client
    /// passes to `PushManager.subscribe` as its `applicationServerKey`.
    public: String,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl Key {
    /// Reads the key from `dir`, or generates it there on first start.
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        let path = dir.join(FILE);
        private_dir(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => Self::load(&path, &metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = Self::generate()?;
                key.save(&path)
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
                tracing::info!(path = %path.display(), "generated the gateway's VAPID key");
                Ok(key)
            }
            Err(error) => Err(format!("reading {}: {error}", path.display())),
        }
    }

    fn load(path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt;
        let broken = |why: &str| {
            format!(
                "{} {why}. Every push subscription depends on this key, so the gateway \
                 will not replace it: restore it, or move it aside to make a new key and \
                 end every subscription",
                path.display()
            )
        };
        if !metadata.is_file() {
            return Err(broken("is not a regular file"));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(broken("is readable by others; it must have mode 600"));
        }
        let pem = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|e| broken(&format!("cannot be read: {e}")))?,
        );
        let secret = SecretKey::from_pkcs8_pem(&pem)
            .map_err(|_| broken("is not a P-256 private key in PKCS#8 PEM"))?;
        Ok(Self::from_secret(&secret))
    }

    /// A new random key.
    pub fn generate() -> Result<Self, String> {
        // A random 32-byte string is a valid scalar unless it is zero or at
        // least the group order, which is vanishingly rare.
        for _ in 0..8 {
            let mut bytes = Zeroizing::new([0u8; 32]);
            getrandom::fill(bytes.as_mut()).map_err(|e| format!("no randomness: {e}"))?;
            if let Ok(secret) = SecretKey::from_slice(bytes.as_ref()) {
                return Ok(Self::from_secret(&secret));
            }
        }
        Err("could not generate a VAPID key".into())
    }

    fn from_secret(secret: &SecretKey) -> Self {
        let signing = SigningKey::from(secret);
        let point = signing.verifying_key().to_encoded_point(false);
        Self {
            public: URL_SAFE_NO_PAD.encode(point.as_bytes()),
            signing,
        }
    }

    /// Writes the key to `path` at mode 600: to a temporary file first,
    /// synced, then renamed into place.
    fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let pem = SecretKey::from(self.signing.as_nonzero_scalar())
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(std::io::Error::other)?;
        let temporary = path.with_extension("pem.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(pem.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }

    /// The public key, uncompressed, in unpadded base64url.
    pub fn public_key(&self) -> &str {
        &self.public
    }

    /// A token for the push service at `audience` (its origin), naming
    /// `contact` and expiring [`TOKEN_LIFETIME_MS`] after `now`.
    pub fn token(&self, audience: &str, contact: &str, now: Timestamp) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": audience,
            "exp": (now.0 + TOKEN_LIFETIME_MS) / 1000,
            "sub": contact,
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let input = format!("{header}.{claims}");
        let signature: Signature = self.signing.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }

    /// The `Authorization` header value for a token.
    pub fn authorization(&self, token: &str) -> String {
        format!("vapid t={token}, k={}", self.public)
    }
}

/// Makes `dir` (and its parents) and gives it mode 700.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// The push directory under the data directory.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("push")
}

/// Tokens made, by push service origin.
#[derive(Debug, Default)]
pub struct Tokens {
    made: HashMap<String, Made>,
}

/// A token made for one push service.
#[derive(Debug)]
struct Made {
    /// The `Authorization` header that carries it.
    header: String,
    /// When it was made.
    at: i64,
    /// Whether the push service refused it.
    refused: bool,
}

impl Made {
    /// Whether it may still be used at `now`: for [`TOKEN_REUSE_MS`], or
    /// only [`TOKEN_RENEWAL_MS`] once refused. A clock that went back
    /// makes a new one.
    fn usable(&self, now: Timestamp) -> bool {
        let reuse = if self.refused {
            TOKEN_RENEWAL_MS
        } else {
            TOKEN_REUSE_MS
        };
        now.0 >= self.at && now.0 - self.at < reuse
    }
}

impl Tokens {
    /// The `Authorization` header for a request to the push service at
    /// `audience`: the cached token while it is usable, else a new one.
    pub fn authorization(
        &mut self,
        key: &Key,
        audience: &str,
        contact: &str,
        now: Timestamp,
    ) -> String {
        if let Some(made) = self.made.get(audience)
            && made.usable(now)
        {
            return made.header.clone();
        }
        if self.made.len() >= CACHED_TOKENS {
            self.made.retain(|_, made| made.usable(now));
            if self.made.len() >= CACHED_TOKENS {
                self.made.clear();
            }
        }
        let header = key.authorization(&key.token(audience, contact, now));
        self.made.insert(
            audience.to_owned(),
            Made {
                header: header.clone(),
                at: now.0,
                refused: false,
            },
        );
        header
    }

    /// Records that the push service at `audience` refused the token in
    /// `header`, so that it is replaced once it is [`TOKEN_RENEWAL_MS`]
    /// old. A refusal of a token already replaced changes nothing.
    pub fn refused(&mut self, audience: &str, header: &str) {
        if let Some(made) = self.made.get_mut(audience)
            && made.header == header
        {
            made.refused = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_key_is_made_once_privately_and_read_back_unchanged() {
        let data = tempfile::tempdir().unwrap();
        let dir = dir(data.path());
        let made = Key::load_or_create(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(FILE)), 0o600);
        assert!(!dir.join("vapid.pem.tmp").exists());
        let pem = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"), "{pem}");
        let again = Key::load_or_create(&dir).unwrap();
        assert_eq!(made.public_key(), again.public_key());
        // The public key is an uncompressed P-256 point.
        let point = URL_SAFE_NO_PAD.decode(made.public_key()).unwrap();
        assert_eq!(point.len(), 65);
        assert_eq!(point[0], 4);
        // Debug output shows the public key only.
        let shown = format!("{made:?}");
        assert!(shown.contains(made.public_key()));
        assert!(!shown.contains("PRIVATE"));
    }

    #[test]
    fn a_broken_or_exposed_key_is_refused_and_left_alone() {
        let data = tempfile::tempdir().unwrap();
        let dir = dir(data.path());
        Key::load_or_create(&dir).unwrap();
        let path = dir.join(FILE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = Key::load_or_create(&dir).unwrap_err();
        assert!(error.contains("mode 600"), "{error}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, "not a key").unwrap();
        let error = Key::load_or_create(&dir).unwrap_err();
        assert!(error.contains("not a P-256 private key"), "{error}");
        assert!(!error.contains("not a key\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a key");
    }

    #[test]
    fn tokens_are_signed_es256_jwts_for_the_push_service() {
        let key = Key::generate().unwrap();
        let now = Timestamp(1_800_000_000_000);
        let token = key.token("https://push.example", "https://bokkie.example", now);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example");
        assert_eq!(claims["sub"], "https://bokkie.example");
        assert_eq!(claims["exp"], (now.0 + TOKEN_LIFETIME_MS) / 1000);
        let point = URL_SAFE_NO_PAD.decode(key.public_key()).unwrap();
        let verifying = VerifyingKey::from_sec1_bytes(&point).unwrap();
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        verifying
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn a_token_is_reused_for_hours_per_push_service() {
        let key = Key::generate().unwrap();
        let mut tokens = Tokens::default();
        let start = Timestamp(1_800_000_000_000);
        let contact = "mailto:owner@example.org";
        let first = tokens.authorization(&key, "https://a.example", contact, start);
        assert!(first.starts_with("vapid t="));
        assert!(first.ends_with(&format!(", k={}", key.public_key())));
        let later = Timestamp(start.0 + TOKEN_REUSE_MS - 1);
        assert_eq!(
            tokens.authorization(&key, "https://a.example", contact, later),
            first
        );
        let other = tokens.authorization(&key, "https://b.example", contact, later);
        assert_ne!(other, first);
        let renewed = Timestamp(start.0 + TOKEN_REUSE_MS);
        assert_ne!(
            tokens.authorization(&key, "https://a.example", contact, renewed),
            first
        );
    }

    #[test]
    fn a_refused_token_is_replaced_after_an_hour_not_sooner() {
        let key = Key::generate().unwrap();
        let mut tokens = Tokens::default();
        let start = Timestamp(1_800_000_000_000);
        let contact = "mailto:owner@example.org";
        let at = |ms: i64| Timestamp(start.0 + ms);
        let first = tokens.authorization(&key, "https://a.example", contact, start);
        let other = tokens.authorization(&key, "https://b.example", contact, start);
        tokens.refused("https://a.example", &first);
        // Within the hour the refused token is still the one sent.
        assert_eq!(
            tokens.authorization(&key, "https://a.example", contact, at(TOKEN_RENEWAL_MS - 1)),
            first
        );
        // Then a new one, which is reused as any other.
        let renewed =
            tokens.authorization(&key, "https://a.example", contact, at(TOKEN_RENEWAL_MS));
        assert_ne!(renewed, first);
        assert_eq!(
            tokens.authorization(&key, "https://a.example", contact, at(2 * TOKEN_RENEWAL_MS)),
            renewed
        );
        // Another push service's token is untouched.
        assert_eq!(
            tokens.authorization(&key, "https://b.example", contact, at(2 * TOKEN_RENEWAL_MS)),
            other
        );
        // A late refusal of the replaced token does not end the new one.
        tokens.refused("https://a.example", &first);
        assert_eq!(
            tokens.authorization(&key, "https://a.example", contact, at(3 * TOKEN_RENEWAL_MS)),
            renewed
        );
    }
}
