use crate::configs::Settings;
use actix_web::cookie::{Cookie, SameSite};
use chrono::{Duration, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode,
    errors::{Error, ErrorKind},
};
use std::time::Duration as StdDuration;

pub(crate) const ACCESS_TOKEN_EXPIRY_HOURS: u64 = 8;
pub(crate) const REFRESH_TOKEN_EXPIRY_HOURS: u64 = 90 * 24; // 90 days
pub(crate) const ACCESS_TOKEN_COOKIE_KEY: &str = "access_token";
pub(crate) const REFRESH_TOKEN_COOKIE_KEY: &str = "refresh_token";

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct AccessClaims {
    pub exp: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct RefreshClaims {
    pub exp: usize,
}

pub fn create_access_token(settings: &Settings) -> Result<String, jsonwebtoken::errors::Error> {
    let access_expiration = (Utc::now()
        + Duration::hours(ACCESS_TOKEN_EXPIRY_HOURS.try_into().unwrap()))
    .timestamp() as usize;
    let access_claims = AccessClaims {
        exp: access_expiration,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &access_claims,
        &EncodingKey::from_secret(settings.auth_access_token_secret.as_ref()),
    )
}

pub fn create_refresh_token(settings: &Settings) -> Result<String, jsonwebtoken::errors::Error> {
    let refresh_expiration = (Utc::now()
        + Duration::hours(REFRESH_TOKEN_EXPIRY_HOURS.try_into().unwrap()))
    .timestamp() as usize;
    let refresh_claims = RefreshClaims {
        exp: refresh_expiration,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &refresh_claims,
        &EncodingKey::from_secret(settings.auth_refresh_token_secret.as_ref()),
    )
}

pub fn create_access_cookie(value: String) -> Cookie<'static> {
    Cookie::build(ACCESS_TOKEN_COOKIE_KEY, value)
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .max_age(
            StdDuration::from_secs(ACCESS_TOKEN_EXPIRY_HOURS * 3600)
                .try_into()
                .unwrap(),
        )
        .finish()
}

pub fn create_clear_cookie(key: &str) -> Cookie<'static> {
    Cookie::build(key.to_string(), "")
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .max_age(
            StdDuration::from_secs(0)
                .try_into()
                .unwrap(),
        )
        .finish()
}

pub fn create_refresh_cookie(value: String) -> Cookie<'static> {
    Cookie::build(REFRESH_TOKEN_COOKIE_KEY, value)
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .max_age(
            StdDuration::from_secs(REFRESH_TOKEN_EXPIRY_HOURS * 3600)
                .try_into()
                .unwrap(),
        )
        .finish()
}

pub fn decode_access_token(token: &str, settings: &Settings) -> Result<AccessClaims, Error> {
    let validation = Validation::new(Algorithm::HS256);
    let token_data = decode::<AccessClaims>(
        token,
        &DecodingKey::from_secret(settings.auth_access_token_secret.as_ref()),
        &validation,
    )?;

    if token_data.claims.exp < Utc::now().timestamp() as usize {
        return Err(Error::from(ErrorKind::ExpiredSignature));
    }

    Ok(token_data.claims)
}
