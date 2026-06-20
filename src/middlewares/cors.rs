use crate::configs::Settings;
use actix_cors::Cors;
use actix_web::http::header::{self, HeaderName};

fn cors_allowed_headers() -> Vec<HeaderName> {
    vec![
        header::ACCEPT,
        header::ACCEPT_ENCODING,
        header::ACCEPT_LANGUAGE,
        header::AUTHORIZATION,
        header::CACHE_CONTROL,
        header::CONTENT_LENGTH,
        header::CONTENT_TYPE,
        header::IF_MATCH,
        header::IF_MODIFIED_SINCE,
        header::IF_NONE_MATCH,
        header::PRAGMA,
        header::RANGE,
        HeaderName::from_static("baggage"),
        HeaderName::from_static("sentry-trace"),
        HeaderName::from_static("x-client-version"),
        HeaderName::from_static("x-idempotency-key"),
        HeaderName::from_static("x-request-id"),
        HeaderName::from_static("x-requested-with"),
        HeaderName::from_static("x-share-secret"),
    ]
}

pub fn cors_middleware(settings: &Settings) -> Cors {
    let mut cors = Cors::default()
        .supports_credentials()
        .allowed_methods(vec!["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"])
        .allowed_headers(cors_allowed_headers())
        .max_age(3600);

    for origin in &settings.cors_allowed_origins {
        cors = cors.allowed_origin(origin);
    }

    cors
}
