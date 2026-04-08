use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Generate a lease extension token by HMAC-SHA256 signing the lease reference.
/// Mirrors `modules/crypto/lease_token.js`.
pub fn sign_lease_token(lease_ref: &str, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(lease_ref.as_bytes());
    let result = mac.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(result.into_bytes())
}

/// Verify a lease extension token.
pub fn verify_lease_token(lease_ref: &str, token: &str, secret: &str) -> bool {
    let expected = sign_lease_token(lease_ref, secret);
    // Constant-time comparison
    use subtle::ConstantTimeEq;
    expected.as_bytes().ct_eq(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_verify_roundtrip() {
        let secret = "test-secret-key";
        let lease_ref = "peer_42";
        let token = sign_lease_token(lease_ref, secret);
        assert!(verify_lease_token(lease_ref, &token, secret));
        assert!(!verify_lease_token(lease_ref, "tampered", secret));
        assert!(!verify_lease_token("wrong_ref", &token, secret));
    }
}
