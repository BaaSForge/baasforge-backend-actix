use actix_web::{
    http::{header::ContentType, StatusCode},
    HttpResponse, ResponseError,
};
use serde_json::json;
use std::fmt;

#[derive(Debug)]
pub enum ApiError {
  NotFound(String),
}

impl fmt::Display for ApiError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ApiError::NotFound(msg) => write!(f, "Not Found: {}", msg),
    }
  }
}

impl ResponseError for ApiError {
  fn status_code(&self) -> StatusCode {
    match *self {
      ApiError::NotFound(_) => StatusCode::NOT_FOUND,
    }
  }

  fn error_response(&self) -> HttpResponse {
    let status_code = self.status_code();
    let json_error = json!({
      "success": false,
      "message": self.to_string(),
    });

    HttpResponse::build(status_code)
      .insert_header(ContentType::json())
      .body(json_error.to_string())
  }
}