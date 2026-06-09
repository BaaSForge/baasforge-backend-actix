use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};

pub fn generate_code_verifier() -> String {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .expect("system RNG must provide entropy");
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn pkce_challenge(verifier: &str) -> String {
    let hash = digest::digest(&digest::SHA256, verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash.as_ref())
}
