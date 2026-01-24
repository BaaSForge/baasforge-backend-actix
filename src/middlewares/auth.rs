use diesel::prelude::*;
use actix_web::{
    Error, HttpMessage, HttpResponse, body::BoxBody, dev::{ServiceRequest, ServiceResponse}, middleware::Next, web
};
use crate::{
    models::user_tokens::{UserToken, UserTokenType}, routes::AppContext, schema::user_tokens, services::auth::{ACCESS_TOKEN_COOKIE_KEY, decode_access_token}
};
use crate::schema::user_tokens::{token, token_type};

pub async fn auth_closed_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    let token_str = if let Some(cookie) = req.cookie(ACCESS_TOKEN_COOKIE_KEY) {
        cookie.value().to_string()
    } else {
        return Ok(req.into_response(HttpResponse::Unauthorized().finish()));
    };

    let app_context = req
        .app_data::<web::Data<AppContext>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

    let mut conn = app_context.pool.get().expect("Failed to get DB connection");

    let claims = decode_access_token(&token_str, &app_context.settings)
        .map_err(|_| actix_web::error::ErrorUnauthorized("Invalid token"))?;

    let db_token: Option<UserToken> = user_tokens::table
        .filter(token.eq(&token_str))
        .filter(token_type.eq(UserTokenType::Access))
        .select(UserToken::as_select()) 
        .first::<UserToken>(&mut conn)
        .optional()
        .map_err(|e| {
            log::error!("DB error: {:?}", e);
            actix_web::error::ErrorInternalServerError("DB error")
        })?;

    if let Some(db_token) = db_token {
        if db_token.revoked.unwrap_or(false) {
            return Ok(req.into_response(HttpResponse::Unauthorized().finish()));
        }
    } else {
        return Ok(req.into_response(HttpResponse::Unauthorized().finish()));
    }

    req.extensions_mut().insert(claims);
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