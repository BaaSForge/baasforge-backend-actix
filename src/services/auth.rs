use chrono::{Utc, Duration};
use jsonwebtoken::{encode, EncodingKey, Header, Algorithm};
use crate::configs::Settings;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct AccessClaims {
  pub exp: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct RefreshClaims {
  pub exp: usize,
}

pub fn create_access_token(settings: &Settings) -> Result<String, jsonwebtoken::errors::Error> {
  let access_expiration = (Utc::now() + Duration::hours(8)).timestamp() as usize;
  let access_claims = AccessClaims {
    exp: access_expiration,
  };
  encode(&Header::new(Algorithm::HS256), &access_claims, &EncodingKey::from_secret(settings.auth_access_token_secret.as_ref()))
}

pub fn create_refresh_token(settings: &Settings) -> Result<String, jsonwebtoken::errors::Error> {
  let refresh_expiration = (Utc::now() + Duration::days(90)).timestamp() as usize;
  let refresh_claims = RefreshClaims {
    exp: refresh_expiration,
  };
  encode(&Header::new(Algorithm::HS256), &refresh_claims, &EncodingKey::from_secret(settings.auth_refresh_token_secret.as_ref()))
}