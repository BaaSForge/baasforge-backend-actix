use crate::configs::Settings;
use actix_web::cookie::{Cookie, SameSite};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode,
    errors::{Error, ErrorKind},
};
use std::time::Duration as StdDuration;

pub(crate) const ACCESS_TOKEN_EXPIRY_MINUTES: u64 = 60 * 8; // 8 hours
pub(crate) const REFRESH_TOKEN_EXPIRY_MINUTES: u64 = 60 * 24 * 90; // 90 days
pub(crate) const ACCESS_TOKEN_COOKIE_KEY: &str = "access_token";
pub(crate) const REFRESH_TOKEN_COOKIE_KEY: &str = "refresh_token";

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct AccessClaims {
    pub exp: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct RefreshClaims {
    pub exp: usize,
    pub account_created_at: i64,
    pub token_ordinal: i32,
    pub token_created_at: i64,
    pub is_signup_token: bool,
}

pub fn create_access_token(settings: &Settings) -> Result<String, jsonwebtoken::errors::Error> {
    let access_expiration = (Utc::now()
        + Duration::minutes(ACCESS_TOKEN_EXPIRY_MINUTES.try_into().unwrap()))
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

pub fn create_refresh_token(
    settings: &Settings,
    account_created_at: DateTime<Utc>,
    token_ordinal: i32,
    token_created_at: DateTime<Utc>,
    is_signup_token: bool,
) -> Result<String, jsonwebtoken::errors::Error> {
    let refresh_expiration = (Utc::now()
        + Duration::minutes(REFRESH_TOKEN_EXPIRY_MINUTES.try_into().unwrap()))
    .timestamp() as usize;
    let refresh_claims = RefreshClaims {
        exp: refresh_expiration,
        account_created_at: account_created_at.timestamp(),
        token_ordinal,
        token_created_at: token_created_at.timestamp(),
        is_signup_token,
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
            StdDuration::from_mins(ACCESS_TOKEN_EXPIRY_MINUTES)
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
        .max_age(StdDuration::from_mins(0).try_into().unwrap())
        .finish()
}

pub fn create_refresh_cookie(value: String) -> Cookie<'static> {
    Cookie::build(REFRESH_TOKEN_COOKIE_KEY, value)
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .max_age(
            StdDuration::from_mins(REFRESH_TOKEN_EXPIRY_MINUTES)
                .try_into()
                .unwrap(),
        )
        .finish()
}

pub fn decode_access_token(token_str: &str, settings: &Settings) -> Result<AccessClaims, Error> {
    let validation = Validation::new(Algorithm::HS256);
    let token_data = decode::<AccessClaims>(
        token_str,
        &DecodingKey::from_secret(settings.auth_access_token_secret.as_ref()),
        &validation,
    )?;

    if token_data.claims.exp < Utc::now().timestamp() as usize {
        return Err(Error::from(ErrorKind::ExpiredSignature));
    }

    Ok(token_data.claims)
}

pub fn decode_refresh_token(token_str: &str, settings: &Settings) -> Result<RefreshClaims, Error> {
    let validation = Validation::new(Algorithm::HS256);
    let token_data = decode::<RefreshClaims>(
        token_str,
        &DecodingKey::from_secret(settings.auth_refresh_token_secret.as_ref()),
        &validation,
    )?;

    if token_data.claims.exp < Utc::now().timestamp() as usize {
        return Err(Error::from(ErrorKind::ExpiredSignature));
    }

    Ok(token_data.claims)
}

pub fn is_token_mature_enough(claims: &RefreshClaims) -> Result<(), String> {
    let now = Utc::now();
    let account_created_at = DateTime::<Utc>::from_timestamp(claims.account_created_at, 0)
        .ok_or("Invalid account_created_at timestamp")?;
    let token_created_at = DateTime::<Utc>::from_timestamp(claims.token_created_at, 0)
        .ok_or("Invalid token_created_at timestamp")?;
    
    let account_age = now - account_created_at;
    let token_age = now - token_created_at;
    
    if account_age < Duration::days(7) {
        return Ok(());
    }
    
    if claims.is_signup_token {
        return Ok(());
    }
    
    if claims.token_ordinal == 2 || claims.token_ordinal == 3 {
        return Ok(());
    }
    
    if token_age >= Duration::days(2) {
        return Ok(());
    }
    
    let wait_time = Duration::days(2) - token_age;
    let hours = wait_time.num_hours();
    let minutes = wait_time.num_minutes() % 60;
    
    let wait_message = if hours > 24 {
        let days = hours / 24;
        let remaining_hours = hours % 24;
        if remaining_hours > 0 {
            format!("{} days and {} hours", days, remaining_hours)
        } else {
            format!("{} days", days)
        }
    } else if hours > 0 {
        if minutes > 0 {
            format!("{} hours and {} minutes", hours, minutes)
        } else {
            format!("{} hours", hours)
        }
    } else {
        format!("{} minutes", minutes)
    };
    
    Err(format!(
        "This token is beyond your 2nd and 3rd tokens and your account is older than 7 days. This token needs to mature for {} before performing critical actions.",
        wait_message
    ))
}
