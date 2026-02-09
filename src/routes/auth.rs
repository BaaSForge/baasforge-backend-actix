use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::{AccessPayload, RefreshPayload};
// use actix_web::HttpMessage;
use crate::models::user_tokens::{UserTokenInternal, UserTokenResponse, UserTokenType};
use crate::models::users::{PlatformType, User, UserDTO};
use crate::routes::AppContext;
use crate::schema::users::{full_name, id_in_platform, platform};
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
        Some(url) => format!("/api/v1/auth/google/callback?callback-url={}", urlencoding::encode(url)),
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

    let mut conn = context.pool.get().expect("Failed to get DB connection");

    let existing_user = users::table
        .filter(platform.eq(PlatformType::Google))
        .filter(id_in_platform.eq(id_in_platform_dto))
        .first::<User>(&mut conn)
        .optional()
        .unwrap();

    let mut is_just_signed_up = false;

    let user: User = if let Some(u) = existing_user {
        diesel::update(users::table.find(u.id))
            .set(full_name.eq(Some(full_name_dto.to_string())))
            .get_result(&mut conn)
            .unwrap()
    } else {
        is_just_signed_up = true;

        diesel::insert_into(users::table)
            .values(&UserDTO {
                platform: PlatformType::Google,
                id_in_platform: id_in_platform_dto.to_string(),
                full_name: Some(full_name_dto.to_string()),
            })
            .get_result(&mut conn)
            .unwrap()
    };

    let now = Utc::now();
    let access_exp = now + Duration::minutes(ACCESS_TOKEN_EXPIRY_MINUTES as i64);
    let refresh_exp = now + Duration::minutes(REFRESH_TOKEN_EXPIRY_MINUTES as i64);

    let access_token_cookie = create_access_cookie(access_token.clone());
    let refresh_token_cookie = create_refresh_cookie(refresh_token.clone());

    let refresh_token_row = UserTokenInternal {
        user_id: user.id,
        token_type: UserTokenType::Refresh,
        token: refresh_token,
        expires_at: refresh_exp,
        revoked: Some(false),
        is_signup_token: is_just_signed_up,
        refresh_token_id: None,
    };
    let refresh_token_id: i64 = diesel::insert_into(user_tokens::table)
        .values(&refresh_token_row)
        .returning(user_tokens::id)
        .get_result(&mut conn)
        .expect("Failed to insert refresh token");

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
        .execute(&mut conn)
        .expect("Failed to insert access token");

    HttpResponse::Found()
        .append_header(("Location", query.callback_url.clone()))
        .cookie(access_token_cookie)
        .cookie(refresh_token_cookie)
        .finish()
}

pub async fn handle_logout(req: HttpRequest, context: web::Data<AppContext>) -> HttpResponse {
    let mut conn = context.pool.get().unwrap();

    let access = req.cookie(ACCESS_TOKEN_COOKIE_KEY);
    let refresh = req.cookie(REFRESH_TOKEN_COOKIE_KEY);

    if let (Some(a), Some(r)) = (access, refresh) {
        diesel::update(user_tokens::table)
            .filter(user_tokens::token.eq_any(vec![a.value().to_string(), r.value().to_string()]))
            .set(user_tokens::revoked.eq(Some(true)))
            .execute(&mut conn)
            .ok();
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
    let exp = Utc::now() + Duration::minutes(ACCESS_TOKEN_EXPIRY_MINUTES as i64);

    let mut conn = context.pool.get().expect("DB conn");

    diesel::insert_into(user_tokens::table)
        .values(&UserTokenInternal {
            user_id: refresh_payload.user_id,
            token_type: UserTokenType::Access,
            token: access.clone(),
            expires_at: exp,
            revoked: Some(false),
            is_signup_token: false,
            refresh_token_id: Some(refresh_payload.db_token.id),
        })
        .execute(&mut conn)
        .unwrap();

    HttpResponse::Ok()
        .cookie(create_access_cookie(access))
        .finish()
}

#[derive(Deserialize)]
pub struct RevokeTokensRequest {
    token_ids: Vec<i64>,
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

    let mut conn = context.pool.get().unwrap();
    let ids = &body.token_ids;

    let tokens: Vec<(i64, UserTokenType)> = user_tokens::table
        .filter(user_tokens::id.eq_any(ids))
        .filter(user_tokens::user_id.eq(refresh_payload.user_id))
        .filter(user_tokens::token.ne(&refresh_payload.db_token.token))
        .filter(user_tokens::token.ne(&access_payload.db_token.token))
        .filter(user_tokens::revoked.eq(Some(false)))
        .select((user_tokens::id, user_tokens::token_type))
        .load::<(i64, UserTokenType)>(&mut conn)
        .unwrap();

    if tokens.len() != ids.len() {
        let api_result = api_error(
            "One or more token IDs do not exist or don't belong to you or are already revoked; current access and refresh tokens cannot be revoked",
            json!({}),
        );
        return HttpResponse::BadRequest().json(api_result);
    }

    let mut revoke_ids: Vec<i64> = Vec::new();
    for (token_id, token_type) in &tokens {
        revoke_ids.push(*token_id);
        if *token_type == UserTokenType::Refresh {
            let access_ids: Vec<i64> = user_tokens::table
                .filter(user_tokens::refresh_token_id.eq(token_id))
                .select(user_tokens::id)
                .load(&mut conn)
                .unwrap_or_default();
            revoke_ids.extend(access_ids);
        }
    }

    if !revoke_ids.is_empty() {
        diesel::update(user_tokens::table.filter(user_tokens::id.eq_any(revoke_ids)))
            .set(user_tokens::revoked.eq(Some(true)))
            .execute(&mut conn)
            .unwrap();
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
    let token_type_filter = query.token_type.clone().map_or(
        vec![UserTokenType::Access, UserTokenType::Refresh],
        |t| vec![t],
    );

    let mut conn = context.pool.get().expect("DB conn");

    let tokens = user_tokens::table
        .filter(user_tokens::user_id.eq(access_payload.user_id))
        .filter(user_tokens::revoked.eq(Some(revoked_filter)))
        .filter(user_tokens::token_type.eq_any(token_type_filter))
        .filter(user_tokens::token.ne(&refresh_payload.db_token.token))
        .filter(user_tokens::token.ne(&access_payload.db_token.token))
        .select(UserTokenResponse::as_select())
        .load::<UserTokenResponse>(&mut conn)
        .unwrap_or_default();

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

    let mut conn = context.pool.get().expect("DB conn");

    let user = users::table
        .find(access_payload.user_id)
        .first::<User>(&mut conn)
        .optional()
        .unwrap();

    match user {
        Some(u) => {
            HttpResponse::Ok().json(api_success("Current user fetched successfully", json!(u)))
        }
        None => HttpResponse::NotFound().json(api_error("User not found", json!({}))),
    }
}
