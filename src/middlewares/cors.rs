use crate::configs::Settings;
use actix_cors::Cors;
use actix_web::http::header;

pub fn cors_middleware(settings: &Settings) -> Cors {
    let mut cors = Cors::default()
        .supports_credentials()
        .allowed_methods(vec!["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"])
        .allowed_headers(vec![
            header::AUTHORIZATION,
            header::ACCEPT,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-share-secret"),
        ])
        .max_age(3600);

    for origin in &settings.cors_allowed_origins {
        cors = cors.allowed_origin(origin);
    }

    cors
}
