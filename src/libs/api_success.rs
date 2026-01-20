use actix_web::{
  http::{header::ContentType, StatusCode},
  HttpResponse,
};
use serde_json::json;
use std::fmt;

#[derive(Debug)]
pub enum ApiSuccess {
  Ok { message: String, payload: serde_json::Value },
  Created { message: String, payload: serde_json::Value },
}

impl fmt::Display for ApiSuccess {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ApiSuccess::Ok { message, .. } => write!(f, "Ok: {}", message),
      ApiSuccess::Created { message, .. } => write!(f, "Created: {}", message),
    }
  }
}

impl ApiSuccess {
  pub fn into_http_response(self) -> HttpResponse {
    let (status_code, message, payload) = match self {
      ApiSuccess::Ok { message, payload } => (StatusCode::OK, message, payload),
      ApiSuccess::Created { message, payload } => (StatusCode::CREATED, message, payload),
    };

    let json_success = json!({
      "success": true,
      "message": message,
      "payload": payload,
    });

    HttpResponse::build(status_code)
      .insert_header(ContentType::json())
      .body(json_success.to_string())
  }
}