use base64::Engine;
use x25519_dalek::{PublicKey, StaticSecret};

/// A WireGuard key pair (private + public) in base64 encoding.
pub struct WgKeyPair {
    pub private_key: String,
    pub public_key: String,
}

/// Generate a new x25519 key pair for WireGuard.
pub fn generate_keypair() -> WgKeyPair {
    let secret = StaticSecret::random();
    let public = PublicKey::from(&secret);
    let engine = base64::engine::general_purpose::STANDARD;
    WgKeyPair {
        private_key: engine.encode(secret.to_bytes()),
        public_key: engine.encode(public.to_bytes()),
    }
}

/// Generate a random 256-bit preshared key (base64).
pub fn generate_preshared_key() -> String {
    let mut key = [0u8; 32];
    rand::fill(&mut key);
    base64::engine::general_purpose::STANDARD.encode(key)
}

/// Derive the public key from a base64-encoded private key.
pub fn public_key_from_private(private_key_b64: &str) -> anyhow::Result<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine.decode(private_key_b64)?;
    let secret_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid private key length"))?;
    let secret = StaticSecret::from(secret_bytes);
    let public = PublicKey::from(&secret);
    Ok(engine.encode(public.to_bytes()))
}
