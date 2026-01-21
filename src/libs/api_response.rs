use serde::Serialize;

pub type ApiResult<T> = (bool, String, T);

pub fn api_success<T: Serialize>(message: &str, payload: T) -> ApiResult<T> {
  (true, message.to_string(), payload)
}

pub fn api_error<T: Serialize>(message: &str, payload: T) -> ApiResult<T> {
  (false, message.to_string(), payload)
}