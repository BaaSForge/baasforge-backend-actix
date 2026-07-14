use crate::database;
use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::{AccessPayload, RefreshPayload};
use crate::models::user_tokens::{UserTokenResponse, UserTokenType};
use crate::models::users::{CurrentUserResponse, PlatformType, User};
use crate::routes::AppContext;
use crate::schema::{user_tokens, users};
use crate::services::auth::{
    ACCESS_TOKEN_COOKIE_KEY, ACCESS_TOKEN_EXPIRY_MINUTES, REFRESH_TOKEN_COOKIE_KEY,
    create_access_cookie, create_access_token, create_clear_cookie,
};
use crate::services::oauth::{
    GitHubOAuth, GoogleOAuth, OAuthProvider, OAuthUserProfile, complete_oauth_login,
    create_oauth_state, generate_code_verifier, pkce_challenge, verify_oauth_state,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

fn oauth_error_redirect(callback_url: &str, message: &str) -> HttpResponse {
    let location = Url::parse(callback_url)
        .map(|mut url| {
            url.query_pairs_mut().append_pair("auth_error", message);
            url.to_string()
        })
        .unwrap_or_else(|_| callback_url.to_string());
    HttpResponse::Found()
        .append_header(("Location", location))
        .finish()
}

#[derive(Debug, Deserialize)]
pub struct InitiateGoogleAuthQuery {
    #[serde(rename = "callback-url")]
    pub callback_url: Option<String>,
}

pub async fn initiate_google_auth(
    context: web::Data<AppContext>,
    query: web::Query<InitiateGoogleAuthQuery>,
) -> HttpResponse {
    let Some(callback_url) = query.callback_url.clone() else {
        return HttpResponse::BadRequest()
            .json(api_error("callback-url is required", json!({})));
    };

    let google = match GoogleOAuth::from_settings(&context.settings) {
        Ok(g) => g,
        Err(_) => {
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Google login is not configured",
                json!({}),
            ));
        }
    };

    let code_verifier = generate_code_verifier();
    let state = match create_oauth_state(
        &context.settings,
        OAuthProvider::Google,
        callback_url,
        code_verifier.clone(),
    ) {
        Ok(s) => s,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(api_error("Failed to start Google login", json!({})));
        }
    };

    let location = google.authorization_url(&state, &pkce_challenge(&code_verifier));

    HttpResponse::SeeOther()
        .append_header(("Location", location))
        .finish()
}

#[derive(Debug, Deserialize)]
pub struct CallbackGoogleAuthQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[serde(rename = "error_description")]
    error_description: Option<String>,
}

pub async fn handle_google_auth_callback(
    context: web::Data<AppContext>,
    query: web::Query<CallbackGoogleAuthQuery>,
) -> HttpResponse {
    if let Some(err) = &query.error {
        let message = query
            .error_description
            .as_deref()
            .unwrap_or(err.as_str());
        if let Some(state) = &query.state {
            if let Ok(oauth_state) =
                verify_oauth_state(&context.settings, state, OAuthProvider::Google)
            {
                return oauth_error_redirect(&oauth_state.callback_url, message);
            }
        }
        return HttpResponse::BadRequest().json(api_error(message, json!({})));
    }

    let (code, state) = match (&query.code, &query.state) {
        (Some(code), Some(state)) => (code.as_str(), state.as_str()),
        _ => {
            return HttpResponse::BadRequest()
                .json(api_error("Missing OAuth code or state", json!({})));
        }
    };

    let oauth_state = match verify_oauth_state(&context.settings, state, OAuthProvider::Google) {
        Ok(s) => s,
        Err(_) => {
            return HttpResponse::BadRequest()
                .json(api_error("Invalid or expired OAuth state", json!({})));
        }
    };

    let callback_url = oauth_state.callback_url.clone();
    let code_verifier = oauth_state.code_verifier.clone();

    let google = match GoogleOAuth::from_settings(&context.settings) {
        Ok(g) => g,
        Err(_) => {
            return oauth_error_redirect(&callback_url, "Google login is not configured");
        }
    };

    let access_token = match google.exchange_code(code, &code_verifier).await {
        Ok(t) => t,
        Err(e) => {
            log::error!("[handle_google_auth_callback] token exchange failed: {e}");
            return oauth_error_redirect(&callback_url, "Google login failed");
        }
    };

    let user_info = match google.fetch_user_info(&access_token).await {
        Ok(u) => u,
        Err(e) => {
            log::error!("[handle_google_auth_callback] userinfo failed: {e}");
            return oauth_error_redirect(&callback_url, "Google login failed");
        }
    };

    let profile = OAuthUserProfile {
        platform: PlatformType::Google,
        id_in_platform: user_info.sub,
        full_name: user_info.name,
        email_in_platform: user_info.email,
    };

    let (access_cookie, refresh_cookie) =
        match complete_oauth_login(&context.pool, &context.settings, profile).await {
            Ok(cookies) => cookies,
            Err(_) => {
                log::error!("[handle_google_auth_callback] DB/block error");
                return oauth_error_redirect(&callback_url, "Login failed");
            }
        };

    HttpResponse::Found()
        .append_header(("Location", callback_url))
        .cookie(access_cookie)
        .cookie(refresh_cookie)
        .finish()
}

#[derive(Debug, Deserialize)]
pub struct InitiateGitHubAuthQuery {
    #[serde(rename = "callback-url")]
    pub callback_url: Option<String>,
}

pub async fn initiate_github_auth(
    context: web::Data<AppContext>,
    query: web::Query<InitiateGitHubAuthQuery>,
) -> HttpResponse {
    let Some(callback_url) = query.callback_url.clone() else {
        return HttpResponse::BadRequest()
            .json(api_error("callback-url is required", json!({})));
    };

    let github = match GitHubOAuth::from_settings(&context.settings) {
        Ok(g) => g,
        Err(_) => {
            return HttpResponse::ServiceUnavailable().json(api_error(
                "GitHub login is not configured",
                json!({}),
            ));
        }
    };

    let code_verifier = generate_code_verifier();
    let state = match create_oauth_state(
        &context.settings,
        OAuthProvider::Github,
        callback_url,
        code_verifier.clone(),
    ) {
        Ok(s) => s,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(api_error("Failed to start GitHub login", json!({})));
        }
    };

    let location = github.authorization_url(&state, &pkce_challenge(&code_verifier));

    HttpResponse::SeeOther()
        .append_header(("Location", location))
        .finish()
}

#[derive(Debug, Deserialize)]
pub struct CallbackGitHubAuthQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[serde(rename = "error_description")]
    error_description: Option<String>,
}

pub async fn handle_github_auth_callback(
    context: web::Data<AppContext>,
    query: web::Query<CallbackGitHubAuthQuery>,
) -> HttpResponse {
    if let Some(err) = &query.error {
        let message = query
            .error_description
            .as_deref()
            .unwrap_or(err.as_str());
        if let Some(state) = &query.state {
            if let Ok(oauth_state) =
                verify_oauth_state(&context.settings, state, OAuthProvider::Github)
            {
                return oauth_error_redirect(&oauth_state.callback_url, message);
            }
        }
        return HttpResponse::BadRequest().json(api_error(message, json!({})));
    }

    let (code, state) = match (&query.code, &query.state) {
        (Some(code), Some(state)) => (code.as_str(), state.as_str()),
        _ => {
            return HttpResponse::BadRequest()
                .json(api_error("Missing OAuth code or state", json!({})));
        }
    };

    let oauth_state = match verify_oauth_state(&context.settings, state, OAuthProvider::Github) {
        Ok(s) => s,
        Err(_) => {
            return HttpResponse::BadRequest()
                .json(api_error("Invalid or expired OAuth state", json!({})));
        }
    };

    let callback_url = oauth_state.callback_url.clone();
    let code_verifier = oauth_state.code_verifier.clone();

    let github = match GitHubOAuth::from_settings(&context.settings) {
        Ok(g) => g,
        Err(_) => {
            return oauth_error_redirect(&callback_url, "GitHub login is not configured");
        }
    };

    let access_token = match github.exchange_code(code, &code_verifier).await {
        Ok(t) => t,
        Err(e) => {
            log::error!("[handle_github_auth_callback] token exchange failed: {e}");
            return oauth_error_redirect(&callback_url, "GitHub login failed");
        }
    };

    let user_info = match github.fetch_user_info(&access_token).await {
        Ok(u) => u,
        Err(e) => {
            log::error!("[handle_github_auth_callback] userinfo failed: {e}");
            return oauth_error_redirect(&callback_url, "GitHub login failed");
        }
    };

    let profile = OAuthUserProfile {
        platform: PlatformType::Github,
        id_in_platform: user_info.id,
        full_name: user_info.name,
        email_in_platform: user_info.email,
    };

    let (access_cookie, refresh_cookie) =
        match complete_oauth_login(&context.pool, &context.settings, profile).await {
            Ok(cookies) => cookies,
            Err(_) => {
                log::error!("[handle_github_auth_callback] DB/block error");
                return oauth_error_redirect(&callback_url, "Login failed");
            }
        };

    HttpResponse::Found()
        .append_header(("Location", callback_url))
        .cookie(access_cookie)
        .cookie(refresh_cookie)
        .finish()
}

pub async fn handle_logout(req: HttpRequest, context: web::Data<AppContext>) -> HttpResponse {
    let access = req.cookie(ACCESS_TOKEN_COOKIE_KEY);
    let refresh = req.cookie(REFRESH_TOKEN_COOKIE_KEY);

    let tokens: Vec<String> = match (access, refresh) {
        (Some(a), Some(r)) => vec![a.value().to_string(), r.value().to_string()],
        _ => vec![],
    };

    if !tokens.is_empty() {
        let tokens_clone = tokens.clone();
        let _ = database::block_on_db(&context.pool, move |conn| {
            diesel::update(user_tokens::table)
                .filter(user_tokens::token.eq_any(tokens_clone))
                .set(user_tokens::revoked.eq(Some(true)))
                .execute(conn)
        })
        .await;
    }

    let clear_access = create_clear_cookie(ACCESS_TOKEN_COOKIE_KEY);
    let clear_refresh = create_clear_cookie(REFRESH_TOKEN_COOKIE_KEY);

    HttpResponse::Ok()
        .cookie(clear_access)
        .cookie(clear_refresh)
        .finish()
}

pub async fn handle_refresh_token(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();
    let refresh_payload = extensions
        .get::<RefreshPayload>()
        .expect("RefreshPayload must exist to refresh token");

    let access = create_access_token(&context.settings).unwrap();
    let access_for_cookie = access.clone();
    let exp = Utc::now() + Duration::minutes(ACCESS_TOKEN_EXPIRY_MINUTES as i64);
    let user_id = refresh_payload.user_id;
    let refresh_token_id = refresh_payload.db_token.id;

    match database::block_on_db(&context.pool, move |conn| {
        diesel::insert_into(user_tokens::table)
            .values(&crate::models::user_tokens::UserTokenInternal {
                user_id,
                token_type: UserTokenType::Access,
                token: access,
                expires_at: exp,
                revoked: Some(false),
                is_signup_token: false,
                refresh_token_id: Some(refresh_token_id),
            })
            .execute(conn)
    })
    .await
    {
        Ok(_) => {}
        Err(_) => {
            log::error!("[handle_refresh_token] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok()
        .cookie(create_access_cookie(access_for_cookie))
        .finish()
}

#[derive(Deserialize)]
pub struct RevokeTokensRequest {
    token_ids: Vec<uuid::Uuid>,
}

pub async fn handle_revoke_tokens(
    context: web::Data<AppContext>,
    req: HttpRequest,
    body: web::Json<RevokeTokensRequest>,
) -> HttpResponse {
    let extensions = req.extensions();
    let refresh_payload = extensions
        .get::<RefreshPayload>()
        .expect("RefreshPayload must exist to revoke tokens");

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to revoke tokens");

    let ids = body.token_ids.clone();
    let user_id = refresh_payload.user_id;
    let refresh_token = refresh_payload.db_token.token.clone();
    let access_token = access_payload.db_token.token.clone();

    enum RevokeOut {
        Ok,
        TokenMismatch,
    }
    let revoke_result = database::block_on_db(&context.pool, move |conn| {
        let tokens: Vec<(uuid::Uuid, UserTokenType)> = user_tokens::table
            .filter(user_tokens::id.eq_any(&ids))
            .filter(user_tokens::user_id.eq(user_id))
            .filter(user_tokens::token.ne(&refresh_token))
            .filter(user_tokens::token.ne(&access_token))
            .filter(user_tokens::revoked.eq(Some(false)))
            .select((user_tokens::id, user_tokens::token_type))
            .load::<(uuid::Uuid, UserTokenType)>(conn)?;

        if tokens.len() != ids.len() {
            return Ok(RevokeOut::TokenMismatch);
        }

        let mut revoke_ids: Vec<uuid::Uuid> = Vec::new();
        for (token_id, token_type) in &tokens {
            revoke_ids.push(*token_id);
            if *token_type == UserTokenType::Refresh {
                let access_ids: Vec<uuid::Uuid> = user_tokens::table
                    .filter(user_tokens::refresh_token_id.eq(token_id))
                    .select(user_tokens::id)
                    .load(conn)
                    .unwrap_or_default();
                revoke_ids.extend(access_ids);
            }
        }

        if !revoke_ids.is_empty() {
            diesel::update(user_tokens::table.filter(user_tokens::id.eq_any(revoke_ids)))
                .set(user_tokens::revoked.eq(Some(true)))
                .execute(conn)?;
        }
        Ok(RevokeOut::Ok)
    })
    .await;

    match revoke_result {
        Ok(RevokeOut::Ok) => {}
        Ok(RevokeOut::TokenMismatch) => {
            return HttpResponse::BadRequest().json(api_error(
                "One or more token IDs do not exist or don't belong to you or are already revoked; current access and refresh tokens cannot be revoked",
                json!({}),
            ));
        }
        Err(_) => {
            log::error!("[handle_revoke_tokens] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    }

    HttpResponse::Ok().json(api_success("Tokens revoked successfully", json!({})))
}

#[derive(Debug, Deserialize)]
pub struct ListTokensQuery {
    pub revoked: Option<bool>,
    #[serde(rename = "type")]
    pub token_type: Option<UserTokenType>,
}

pub async fn handle_list_tokens(
    context: web::Data<AppContext>,
    req: HttpRequest,
    query: web::Query<ListTokensQuery>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list tokens");

    let refresh_payload = extensions
        .get::<RefreshPayload>()
        .expect("RefreshPayload must exist to list tokens");

    let revoked_filter = query.revoked.unwrap_or(false);
    let token_type_filter = query
        .token_type
        .clone()
        .map_or(vec![UserTokenType::Access, UserTokenType::Refresh], |t| {
            vec![t]
        });
    let user_id = access_payload.user_id;
    let refresh_token = refresh_payload.db_token.token.clone();
    let access_token = access_payload.db_token.token.clone();

    let tokens = match database::block_on_db(&context.pool, move |conn| {
        user_tokens::table
            .filter(user_tokens::user_id.eq(user_id))
            .filter(user_tokens::revoked.eq(Some(revoked_filter)))
            .filter(user_tokens::token_type.eq_any(token_type_filter))
            .filter(user_tokens::token.ne(&refresh_token))
            .filter(user_tokens::token.ne(&access_token))
            .select(UserTokenResponse::as_select())
            .load::<UserTokenResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_list_tokens] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "User tokens fetched successfully",
        json!(tokens),
    ))
}

pub async fn handle_get_current_user(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to retrieve current user");

    let user_id = access_payload.user_id;
    let token = UserTokenResponse::from(&access_payload.db_token);

    let user = match database::block_on_db(&context.pool, move |conn| {
        users::table.find(user_id).first::<User>(conn).optional()
    })
    .await
    {
        Ok(u) => u,
        Err(_) => {
            log::error!("[handle_get_current_user] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match user {
        Some(u) => HttpResponse::Ok().json(api_success(
            "Current user fetched successfully",
            CurrentUserResponse { user: u, token },
        )),
        None => HttpResponse::NotFound().json(api_error("User not found", json!({}))),
    }
}
