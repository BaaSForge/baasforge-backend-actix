use crate::configs::Settings;
use chrono::{Duration, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode,
    errors::{Error, ErrorKind},
};
use serde::{Deserialize, Serialize};

const OAUTH_STATE_EXPIRY_MINUTES: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthProvider {
    Google,
    Github,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OAuthStateClaims {
    pub provider: OAuthProvider,
    pub callback_url: String,
    pub code_verifier: String,
    pub exp: usize,
}

fn oauth_state_secret<'a>(
    settings: &'a Settings,
    provider: OAuthProvider,
) -> Result<&'a str, Error> {
    let secret = match provider {
        OAuthProvider::Google => settings.google_oauth_state_secret.as_deref(),
        OAuthProvider::Github => settings.github_oauth_state_secret.as_deref(),
    };
    secret.ok_or_else(|| Error::from(ErrorKind::InvalidToken))
}

pub fn create_oauth_state(
    settings: &Settings,
    provider: OAuthProvider,
    callback_url: String,
    code_verifier: String,
) -> Result<String, Error> {
    let exp = (Utc::now() + Duration::minutes(OAUTH_STATE_EXPIRY_MINUTES)).timestamp() as usize;
    let claims = OAuthStateClaims {
        provider,
        callback_url,
        code_verifier,
        exp,
    };
    let secret = oauth_state_secret(settings, provider)?;
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

pub fn verify_oauth_state(
    settings: &Settings,
    token: &str,
    expected_provider: OAuthProvider,
) -> Result<OAuthStateClaims, Error> {
    let secret = oauth_state_secret(settings, expected_provider)?;
    let validation = Validation::new(Algorithm::HS256);
    let token_data = decode::<OAuthStateClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;

    if token_data.claims.exp < Utc::now().timestamp() as usize {
        return Err(Error::from(ErrorKind::ExpiredSignature));
    }

    if token_data.claims.provider != expected_provider {
        return Err(Error::from(ErrorKind::InvalidToken));
    }

    Ok(token_data.claims)
}
