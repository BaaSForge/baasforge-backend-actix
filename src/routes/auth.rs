use crate::database;
use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::{AccessPayload, RefreshPayload};
// use actix_web::HttpMessage;
use crate::models::user_tokens::{UserTokenInternal, UserTokenResponse, UserTokenType};
use crate::models::users::{PlatformType, User, UserDTO};
use crate::routes::AppContext;
use crate::schema::users::{email_in_platform, full_name, id_in_platform, platform};
use crate::schema::{user_tokens, users};
use crate::services::auth::{
    ACCESS_TOKEN_COOKIE_KEY, ACCESS_TOKEN_EXPIRY_MINUTES, REFRESH_TOKEN_COOKIE_KEY,
    REFRESH_TOKEN_EXPIRY_MINUTES, create_access_cookie, create_access_token, create_clear_cookie,
    create_refresh_cookie, create_refresh_token,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
// use actix_web::{HttpRequest};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

// pub async fn get_user_token(req: HttpRequest) -> Result<HttpResponse, ApiError> {
//   let user_token = req.cookie("token");

//   match user_token {
//     Some(cookie) => {
//       let token = cookie.value().to_string();
//       let response_success = ApiSuccess::Ok {
//         message: "".to_string(),
//         payload: json!({ "token": token }),
//       };

//       Ok(response_success.into_http_response())
//     }
//     None => {
//       Err(ApiError::NotFound("No user token found".to_string()))
//     }
//   }
// }

#[derive(Debug, Deserialize)]
pub struct InitiateGoogleAuthQuery {
    #[serde(rename = "callback-url")]
    pub callback_url: Option<String>,
}

// TODO: implement google OAuth here
pub async fn initiate_google_auth(query: web::Query<InitiateGoogleAuthQuery>) -> HttpResponse {
    let location = match &query.callback_url {
        Some(url) => format!(
            "/api/v1/auth/google/callback?callback-url={}",
            urlencoding::encode(url)
        ),
        None => "/api/v1/auth/google/callback".to_string(),
    };
    HttpResponse::SeeOther()
        .append_header(("Location", location))
        .finish()
}

#[derive(Debug, Deserialize)]
pub struct CallbackGoogleAuthQuery {
    #[serde(rename = "callback-url")]
    pub callback_url: String,
}

// TODO: verify the authenticated user coming from google
pub async fn handle_google_auth_callback(
    context: web::Data<AppContext>,
    query: web::Query<CallbackGoogleAuthQuery>,
) -> HttpResponse {
    let full_name_dto = "something";
    let id_in_platform_dto = "2";
    let email_dto = "user@example.com";

    let access_token_res = create_access_token(&context.settings);
    let refresh_token_res = create_refresh_token(&context.settings);

    let access_token = match access_token_res {
        Ok(t) => t,
        Err(_) => {
            let api_result = api_error("Failed to create access token", json!({}));
            return HttpResponse::InternalServerError().json(api_result);
        }
    };

    let refresh_token = match refresh_token_res {
        Ok(t) => t,
        Err(_) => {
            let api_result = api_error("Failed to create refresh token", json!({}));
            return HttpResponse::InternalServerError().json(api_result);
        }
    };

    let callback_url = query.callback_url.clone();
    let access_token_for_cookie = access_token.clone();
    let refresh_token_for_cookie = refresh_token.clone();

    let (_user, _is_just_signed_up) = match database::block_on_db(&context.pool, move |conn| {
        let existing_user = users::table
            .filter(platform.eq(PlatformType::Google))
            .filter(id_in_platform.eq(id_in_platform_dto))
            .first::<User>(conn)
            .optional()?;

        let (user, is_just_signed_up) = if let Some(u) = existing_user {
            let updated = diesel::update(users::table.find(u.id))
                .set((
                    full_name.eq(Some(full_name_dto.to_string())),
                    email_in_platform.eq(Some(email_dto.to_string())),
                ))
                .get_result::<User>(conn)?;
            (updated, false)
        } else {
            let new_user = diesel::insert_into(users::table)
                .values(&UserDTO {
                    platform: PlatformType::Google,
                    id_in_platform: id_in_platform_dto.to_string(),
                    full_name: Some(full_name_dto.to_string()),
                    email_in_platform: Some(email_dto.to_string()),
                })
                .get_result::<User>(conn)?;
            (new_user, true)
        };

        let now = Utc::now();
        let access_exp = now + Duration::minutes(ACCESS_TOKEN_EXPIRY_MINUTES as i64);
        let refresh_exp = now + Duration::minutes(REFRESH_TOKEN_EXPIRY_MINUTES as i64);

        let refresh_token_row = UserTokenInternal {
            user_id: user.id,
            token_type: UserTokenType::Refresh,
            token: refresh_token,
            expires_at: refresh_exp,
            revoked: Some(false),
            is_signup_token: is_just_signed_up,
            refresh_token_id: None,
        };
        let refresh_token_id: Uuid = diesel::insert_into(user_tokens::table)
            .values(&refresh_token_row)
            .returning(user_tokens::id)
            .get_result::<Uuid>(conn)?;

        let access_token_row = UserTokenInternal {
            user_id: user.id,
            token_type: UserTokenType::Access,
            token: access_token,
            expires_at: access_exp,
            revoked: Some(false),
            is_signup_token: is_just_signed_up,
            refresh_token_id: Some(refresh_token_id),
        };
        diesel::insert_into(user_tokens::table)
            .values(&access_token_row)
            .execute(conn)?;

        Ok::<_, diesel::result::Error>((user, is_just_signed_up))
    })
    .await
    {
        Ok(x) => x,
        Err(_) => {
            log::error!("[handle_google_auth_callback] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let access_token_cookie = create_access_cookie(access_token_for_cookie);
    let refresh_token_cookie = create_refresh_cookie(refresh_token_for_cookie);

    HttpResponse::Found()
        .append_header(("Location", callback_url))
        .cookie(access_token_cookie)
        .cookie(refresh_token_cookie)
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
            .values(&UserTokenInternal {
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

    HttpResponse::Ok().finish()
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
        Some(u) => {
            HttpResponse::Ok().json(api_success("Current user fetched successfully", json!(u)))
        }
        None => HttpResponse::NotFound().json(api_error("User not found", json!({}))),
    }
}
