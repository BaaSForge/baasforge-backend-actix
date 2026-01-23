use crate::libs::api_response::{api_error, api_success};
// use actix_web::HttpMessage;
use crate::models::user_tokens::{UserToken, UserTokenType};
use crate::models::users::{PlatformType, User, UserDTO};
use crate::routes::AppContext;
use crate::schema::users::{full_name, id_in_platform, platform};
use crate::schema::{user_tokens, users};
use crate::services::auth::{
    ACCESS_TOKEN_EXPIRY_HOURS,
    // AccessClaims,
    REFRESH_TOKEN_EXPIRY_HOURS, create_access_cookie,
    create_access_token, create_refresh_cookie, create_refresh_token,
};
use actix_web::web;
// use actix_web::{HttpRequest};
use actix_web::{HttpResponse, get, post};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use serde_json::json;

// #[get("/auth/token")]
// async fn get_user_token(req: HttpRequest) -> Result<HttpResponse, ApiError> {
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

// TODO: implement google OAuth here
#[post("/auth/google/initiate")]
async fn initiate_google_auth() -> HttpResponse {
    HttpResponse::TemporaryRedirect()
        .append_header(("Location", "/api/v1/auth/google/callback"))
        .finish()
}

// TODO: verify the authenticated user coming from google
#[get("/auth/google/callback")]
async fn handle_google_auth_callback(
    // req: HttpRequest,
    context: web::Data<AppContext>,
) -> HttpResponse {
    // let extensions = req.extensions();
    // let claims = match extensions.get::<AccessClaims>() {
    //     Some(c) => c,
    //     None => return HttpResponse::Unauthorized().finish(),
    // };

    let full_name_dto = "something";
    let id_in_platform_dto = "1";

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

    let user: User = if let Some(u) = existing_user {
        diesel::update(users::table.find(u.id))
            .set(full_name.eq(Some(full_name_dto.to_string())))
            .get_result(&mut conn)
            .unwrap()
    } else {
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
    let access_exp = now + Duration::hours(ACCESS_TOKEN_EXPIRY_HOURS as i64);
    let refresh_exp = now + Duration::hours(REFRESH_TOKEN_EXPIRY_HOURS as i64);

    let access_token_cookie = create_access_cookie(access_token.clone());
    let refresh_token_cookie = create_refresh_cookie(refresh_token.clone());

    let new_tokens = vec![
        UserToken {
            user_id: user.id,
            token_type: UserTokenType::Access,
            token: access_token,
            expires_at: access_exp,
            revoked: Some(false),
        },
        UserToken {
            user_id: user.id,
            token_type: UserTokenType::Refresh,
            token: refresh_token,
            expires_at: refresh_exp,
            revoked: Some(false),
        },
    ];

    let _ = diesel::insert_into(user_tokens::table)
        .values(new_tokens)
        .execute(&mut conn)
        .expect("Failed to insert user tokens");

    HttpResponse::Created()
        .cookie(access_token_cookie)
        .cookie(refresh_token_cookie)
        .json(api_success("Google authenticated successfully", json!({})))
}
