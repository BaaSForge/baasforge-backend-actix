use crate::database;
use crate::schema::user_tokens::{token, token_type};
use crate::services::auth::{
    AccessClaims, RefreshClaims, decode_refresh_token, is_token_mature_enough,
};
use crate::{
    libs::api_response::api_error,
    models::user_tokens::{UserToken, UserTokenType},
    routes::AppContext,
    schema::user_tokens,
    services::auth::{ACCESS_TOKEN_COOKIE_KEY, REFRESH_TOKEN_COOKIE_KEY, decode_access_token},
};
use actix_web::{
    Error, HttpMessage, HttpResponse,
    body::BoxBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::Next,
    web,
};
use diesel::prelude::*;
use serde_json::json;
use uuid::Uuid;

fn has_refresh_token_cookie(req: &ServiceRequest) -> bool {
    req.cookie(REFRESH_TOKEN_COOKIE_KEY).is_some()
}

fn unauthorized_with_refresh_check(req: ServiceRequest, reason: &str) -> ServiceResponse<BoxBody> {
    log::warn!(
        "[auth] Unauthorized: path={} reason={}",
        req.path(),
        reason
    );
    
    if has_refresh_token_cookie(&req) {
        let body = api_error(
            "You need to refresh the access token",
            json!({"needs_refresh_token": true}),
        );
        req.into_response(HttpResponse::Unauthorized().json(body))
            .map_into_boxed_body()
    } else {
        req.into_response(HttpResponse::Unauthorized().finish())
            .map_into_boxed_body()
    }
}

pub struct AccessPayload {
    #[allow(unused)]
    pub claims: AccessClaims,
    #[allow(unused)]
    pub user_id: Uuid,
    pub db_token: UserToken,
}

pub struct RefreshPayload {
    #[allow(unused)]
    pub claims: RefreshClaims,
    pub user_id: Uuid,
    pub db_token: UserToken,
}

pub async fn auth_closed_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    let token_str = if let Some(cookie) = req.cookie(ACCESS_TOKEN_COOKIE_KEY) {
        cookie.value().to_string()
    } else if req.cookie(REFRESH_TOKEN_COOKIE_KEY).is_some() {
        log::warn!(
            "[auth] Unauthorized: path={} reason=needs_refresh_token",
            req.path()
        );
        let body = api_error(
            "You need to refresh the access token",
            json!({"needs_refresh_token": true}),
        );
        return Ok(req
            .into_response(HttpResponse::Unauthorized().json(body))
            .map_into_boxed_body());
    } else {
        log::warn!(
            "[auth] Unauthorized: path={} reason=no_access_token_cookie (WebSocket needs same-origin or cookie)",
            req.path()
        );
        return Ok(req
            .into_response(HttpResponse::Unauthorized().finish())
            .map_into_boxed_body());
    };

    let app_context = req
        .app_data::<web::Data<AppContext>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

    let claims = match decode_access_token(&token_str, &app_context.settings) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "[auth] Unauthorized: path={} reason=invalid_token decode_err={:?}",
                req.path(),
                e
            );
            return Ok(unauthorized_with_refresh_check(req, "invalid_token"));
        }
    };

    let token_str = token_str.clone();
    let db_token: Option<UserToken> = database::block_on_db(&app_context.pool, move |conn| {
        user_tokens::table
            .filter(token.eq(&token_str))
            .filter(token_type.eq(UserTokenType::Access))
            .select(UserToken::as_select())
            .first::<UserToken>(conn)
            .optional()
    })
    .await
    .map_err(|e| {
        log::error!("[auth] DB/block error: {:?}", e);
        actix_web::error::ErrorServiceUnavailable("Database temporarily unavailable")
    })?;

    let db_token = match db_token {
        Some(t) if !t.revoked.unwrap_or(false) => t,
        _ => {
            return Ok(unauthorized_with_refresh_check(req, "token_not_found_or_revoked"));
        }
    };

    let payload = AccessPayload {
        claims,
        user_id: db_token.user_id,
        db_token,
    };

    req.extensions_mut().insert(payload);
    let res = next.call(req).await?;
    Ok(res.map_into_boxed_body())
}

pub async fn optional_auth_closed_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    if let Some(cookie) = req.cookie(ACCESS_TOKEN_COOKIE_KEY) {
        let token_str = cookie.value().to_string();
        let app_context = req
            .app_data::<web::Data<AppContext>>()
            .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

        if let Ok(claims) = decode_access_token(&token_str, &app_context.settings) {
            let token_str_db = token_str.clone();
            let db_token: Option<UserToken> =
                database::block_on_db(&app_context.pool, move |conn| {
                    user_tokens::table
                        .filter(token.eq(&token_str_db))
                        .filter(token_type.eq(UserTokenType::Access))
                        .select(UserToken::as_select())
                        .first::<UserToken>(conn)
                        .optional()
                })
                .await
                .ok()
                .flatten();

            if let Some(db_token) = db_token
                && !db_token.revoked.unwrap_or(false)
            {
                req.extensions_mut().insert(AccessPayload {
                    claims,
                    user_id: db_token.user_id,
                    db_token,
                });
            }
        }
    }

    let res = next.call(req).await?;
    Ok(res.map_into_boxed_body())
}

pub async fn auth_closed_refresh_token_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    let token_str = if let Some(cookie) = req.cookie(REFRESH_TOKEN_COOKIE_KEY) {
        cookie.value().to_string()
    } else {
        return Ok(req
            .into_response(HttpResponse::Unauthorized().finish())
            .map_into_boxed_body());
    };

    let app_context = req
        .app_data::<web::Data<AppContext>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

    let claims = decode_refresh_token(&token_str, &app_context.settings)
        .map_err(|_| actix_web::error::ErrorUnauthorized("Invalid refresh token"))?;

    let token_str = token_str.clone();
    let db_token: Option<UserToken> = database::block_on_db(&app_context.pool, move |conn| {
        user_tokens::table
            .filter(token.eq(&token_str))
            .filter(token_type.eq(UserTokenType::Refresh))
            .select(UserToken::as_select())
            .first::<UserToken>(conn)
            .optional()
    })
    .await
    .map_err(|e| {
        log::error!("[auth] DB/block error: {:?}", e);
        actix_web::error::ErrorServiceUnavailable("Database temporarily unavailable")
    })?;

    let db_token = match db_token {
        Some(t) if !t.revoked.unwrap_or(false) => t,
        _ => return Ok(req.into_response(HttpResponse::Unauthorized().finish())),
    };

    let payload = RefreshPayload {
        claims,
        user_id: db_token.user_id,
        db_token,
    };

    req.extensions_mut().insert(payload);
    let res = next.call(req).await?;
    Ok(res.map_into_boxed_body())
}

pub async fn auth_opened_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    if req.cookie(ACCESS_TOKEN_COOKIE_KEY).is_some() {
        return Ok(req.into_response(HttpResponse::Forbidden().finish()));
    }

    let res = next.call(req).await?;
    Ok(res.map_into_boxed_body())
}

pub async fn auth_refresh_token_maturity_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    let maturity_check = {
        let extensions = req.extensions();

        let refresh_payload = extensions
            .get::<RefreshPayload>()
            .expect("RefreshPayload must exist");

        is_token_mature_enough(&refresh_payload.claims)
    };

    if let Err(error_message) = maturity_check {
        let body = api_error(&error_message, json!({}));
        return Ok(req
            .into_response(HttpResponse::Forbidden().json(body))
            .map_into_boxed_body());
    }

    let res = next.call(req).await?;
    Ok(res.map_into_boxed_body())
}
