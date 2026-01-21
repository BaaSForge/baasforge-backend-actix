use actix_web::{get, HttpResponse};
use crate::routes::Settings;
use actix_web::web;
use serde_json::json;
use actix_web::post;
use crate::services::auth::{create_access_token, create_refresh_token};
use crate::libs::api_response::{api_success, api_error};

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
  let response = HttpResponse::TemporaryRedirect()
    .append_header(("Location", "/api/v1/auth/google/callback"))
    .finish();

  response
}

// TODO: verify the authenticated user coming from google
#[get("/auth/google/callback")]
async fn handle_google_auth_callback(settings: web::Data<Settings>) -> HttpResponse {
  let access_token_res = create_access_token(&settings);
  let refresh_token_res = create_refresh_token(&settings);

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

  let payload = json!({
    "access_token": access_token,
    "refresh_token": refresh_token
  });

  let api_result = api_success("Google authentication callback successful", payload);
  HttpResponse::Ok().json(api_result)
}