//! The one-off unlock. A licence key is the buyer's email signed with
//! Ed25519 by the licence server; it is checked here offline against the
//! embedded public key, so the app never phones home.
//!
//! Key format: `ACK1.<base64url email>.<base64url signature>`.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};

/// Where "Buy" sends people. While this is empty the app has no paid tier
/// at all: the limits are off and no Buy or licence UI shows, so a build can
/// never ship a paywall that nobody can pay.
pub const BUY_URL: &str = "";

/// Steps in a sequence without a licence.
pub const FREE_STEPS: usize = 3;
/// Saved profiles without a licence.
pub const FREE_PROFILES: usize = 1;

/// PLACEHOLDER: replace with the licence server's public signing key
/// (base64url, 32 bytes) before BUY_URL is set. It is well formed but no
/// server holds the matching private key.
const PUBLIC_KEY: &str = "IVL40Zt5HSRFMkLhXy6rbLfP-ntqXtMAl5YOBpiB2xI";

const PREFIX: &str = "ACK1";

pub fn paid_tier_enabled() -> bool {
    !BUY_URL.is_empty()
}

/// Returns the licensed email if `key` is genuine.
pub fn verify(key: &str) -> Option<String> {
    let public = URL_SAFE_NO_PAD.decode(PUBLIC_KEY).ok()?;
    verify_with(public.as_slice().try_into().ok()?, key)
}

fn verify_with(public_key: &[u8; 32], key: &str) -> Option<String> {
    // Keys get pasted from emails and web pages: tolerate stray whitespace.
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    let mut parts = key.split('.');
    let (prefix, email, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != PREFIX || parts.next().is_some() {
        return None;
    }
    let email = URL_SAFE_NO_PAD.decode(email).ok()?;
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    let signature = Signature::from_slice(&signature).ok()?;
    let verifying_key = VerifyingKey::from_bytes(public_key).ok()?;
    verifying_key.verify_strict(&email, &signature).ok()?;
    String::from_utf8(email).ok()
}

/// Longest sequence allowed.
pub fn max_steps(licensed: bool) -> usize {
    if licensed || !paid_tier_enabled() {
        usize::MAX
    } else {
        FREE_STEPS
    }
}

/// Most saved profiles allowed.
pub fn max_profiles(licensed: bool) -> usize {
    if licensed || !paid_tier_enabled() {
        usize::MAX
    } else {
        FREE_PROFILES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn issue(signing_key: &SigningKey, email: &str) -> String {
        let signature = signing_key.sign(email.as_bytes());
        format!(
            "{PREFIX}.{}.{}",
            URL_SAFE_NO_PAD.encode(email),
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    #[test]
    fn accepts_a_genuine_key_and_returns_the_email() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public = signing_key.verifying_key().to_bytes();
        let key = issue(&signing_key, "buyer@example.com");
        assert_eq!(verify_with(&public, &key).as_deref(), Some("buyer@example.com"));
        let messy = format!("  {}\n{} ", &key[..20], &key[20..]);
        assert_eq!(verify_with(&public, &messy).as_deref(), Some("buyer@example.com"));
    }

    #[test]
    fn rejects_forged_altered_and_malformed_keys() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public = signing_key.verifying_key().to_bytes();
        let other = SigningKey::from_bytes(&[8u8; 32]);
        assert_eq!(verify_with(&public, &issue(&other, "a@b.c")), None);

        let key = issue(&signing_key, "a@b.c");
        let signature = key.rsplit('.').next().unwrap();
        let swapped = format!("{PREFIX}.{}.{signature}", URL_SAFE_NO_PAD.encode("z@b.c"));
        assert_eq!(verify_with(&public, &swapped), None);

        for bad in ["", "ACK1", "ACK1..", "MAC1.YQ.YQ", "not a key", &format!("{key}.x")] {
            assert_eq!(verify_with(&public, bad), None, "{bad}");
        }
    }

    #[test]
    fn embedded_public_key_is_well_formed() {
        let bytes = URL_SAFE_NO_PAD.decode(PUBLIC_KEY).unwrap();
        let bytes: [u8; 32] = bytes.as_slice().try_into().unwrap();
        assert!(VerifyingKey::from_bytes(&bytes).is_ok());
        assert_eq!(verify("ACK1.YQ.YQ"), None);
    }

    #[test]
    fn limits_only_apply_when_a_paid_tier_exists() {
        assert_eq!(max_steps(true), usize::MAX);
        assert_eq!(max_profiles(true), usize::MAX);
        if paid_tier_enabled() {
            assert_eq!(max_steps(false), FREE_STEPS);
            assert_eq!(max_profiles(false), FREE_PROFILES);
        } else {
            assert_eq!(max_steps(false), usize::MAX);
            assert_eq!(max_profiles(false), usize::MAX);
        }
    }
}
