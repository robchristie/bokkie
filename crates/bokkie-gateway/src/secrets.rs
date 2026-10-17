//! Random tokens, their hashes, and password hashing.

use std::sync::OnceLock;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// The shortest password accepted.
pub const MIN_PASSWORD_CHARS: usize = 10;
/// The longest password accepted, which bounds hashing work.
pub const MAX_PASSWORD_BYTES: usize = 256;

/// A fresh random token: 32 bytes, URL-safe base64.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system provides randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The SHA-256 of a token, which is what the database stores.
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn argon2() -> Argon2<'static> {
    // OWASP's first Argon2id recommendation: 19 MiB, two passes, one lane.
    let params = Params::new(19 * 1024, 2, 1, None).expect("valid Argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Checks a new password against the length policy.
pub fn check_new_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!(
            "Passwords need at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(format!(
            "Passwords may have at most {MAX_PASSWORD_BYTES} bytes"
        ));
    }
    Ok(())
}

/// Hashes a password as an Argon2id PHC string. This is slow by design;
/// call it from a blocking task.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("the operating system provides randomness");
    let salt = SaltString::encode_b64(&salt).expect("16 bytes is a valid salt");
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .expect("Argon2 hashing succeeds with valid parameters")
        .to_string()
}

/// Verifies a password against a stored hash. When there is no stored hash
/// (an unknown username), it verifies against a fixed hash anyway, so both
/// cases take the same time. Call it from a blocking task.
pub fn verify_password(password: &str, stored: Option<&str>) -> bool {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let dummy = DUMMY.get_or_init(|| hash_password("bokkie-dummy-password"));
    if password.len() > MAX_PASSWORD_BYTES {
        return false;
    }
    let Ok(hash) = PasswordHash::new(stored.unwrap_or(dummy)) else {
        return false;
    };
    let ok = argon2().verify_password(password.as_bytes(), &hash).is_ok();
    ok && stored.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_verify_only_against_their_own_hash() {
        let hash = hash_password("correct horse battery");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", Some(&hash)));
        assert!(!verify_password("wrong horse battery", Some(&hash)));
        assert!(!verify_password("correct horse battery", None));
        assert!(!verify_password("x", Some("not a hash")));
    }

    #[test]
    fn tokens_are_random_and_hashes_fixed_width() {
        let a = random_token();
        assert_ne!(a, random_token());
        assert_eq!(a.len(), 43);
        assert_eq!(token_hash(&a).len(), 32);
        assert_eq!(token_hash(&a), token_hash(&a));
    }

    #[test]
    fn password_policy() {
        assert!(check_new_password("short").is_err());
        assert!(check_new_password("long enough password").is_ok());
        assert!(check_new_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
    }
}
