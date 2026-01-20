use actix_web::{get, HttpResponse};
use actix_web::HttpRequest;
use crate::libs::api_error::ApiError;
use crate::libs::api_success::ApiSuccess;
use serde_json::json;

#[get("/token")]
async fn get_user_token(req: HttpRequest) -> Result<HttpResponse, ApiError> {
  let user_token = req.cookie("token");

  match user_token {
    Some(cookie) => {
      let token = cookie.value().to_string();
      let response_success = ApiSuccess::Ok {
        message: "".to_string(),
        payload: json!({ "token": token }),
      };

      Ok(response_success.into_http_response())
    }
    None => {
      Err(ApiError::NotFound("No user token found".to_string()))
    }
  }
}
