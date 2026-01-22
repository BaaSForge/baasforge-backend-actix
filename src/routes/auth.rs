use actix_web::{get, HttpResponse};
use actix_web::web;
use serde_json::json;
use actix_web::post;
use crate::services::auth::{create_access_token, create_refresh_token, create_access_cookie, create_refresh_cookie};
use crate::libs::api_response::{api_success, api_error};
use crate::routes::AppContext;
use crate::schema::users;
use crate::models::user::{User, PlatformType};
use diesel::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

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
async fn handle_google_auth_callback(context: web::Data<AppContext>) -> HttpResponse {
  let access_token_res = create_access_token(&context.settings);
  let refresh_token_res = create_refresh_token(&context.settings);

  let access_token = match access_token_res {
    Ok(t) => t,
    Err(_) => {
      let api_result = api_error("Failed to create access token", json!({}));
      return HttpResponse::InternalServerError().json(api_result);
    },
  };

  let refresh_token = match refresh_token_res {
    Ok(t) => t,
    Err(_) => {
      let api_result = api_error("Failed to create refresh token", json!({}));
      return HttpResponse::InternalServerError().json(api_result);
    },
  };

  let access_token_cookie = create_access_cookie(access_token.clone());
  let refresh_token_cookie = create_refresh_cookie(refresh_token.clone());

  let mut conn = context.pool.get().expect("Failed to get DB connection");

  let timestamp = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .expect("Time went backwards")
    .as_nanos()
    .to_string();

  let _ = diesel::insert_into(users::table)
    .values(&User {
      platform: PlatformType::Google,
      id_in_platform: timestamp,
      full_name: Some("something".to_string()),
    })
    .execute(&mut conn);

  HttpResponse::Created()
    .cookie(access_token_cookie)
    .cookie(refresh_token_cookie)
    .json(api_success("Google authenticated successfully", json!({})))
}