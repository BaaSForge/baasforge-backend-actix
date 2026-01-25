use serde::Serialize;

#[derive(Serialize)]
pub struct ApiResult<T> {
    pub success: bool,
    pub message: String,
    pub data: T,
}

pub fn api_success<T: Serialize>(message: &str, payload: T) -> ApiResult<T> {
    ApiResult {
        success: true,
        message: message.to_string(),
        data: payload,
    }
}

pub fn api_error<T: Serialize>(message: &str, payload: T) -> ApiResult<T> {
    ApiResult {
        success: false,
        message: message.to_string(),
        data: payload,
    }
}
