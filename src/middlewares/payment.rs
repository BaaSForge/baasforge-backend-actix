use crate::database;
use crate::middlewares::auth::AccessPayload;
use crate::models::payments::PaymentResponse;
use crate::routes::AppContext;
use crate::schema::payments;
use crate::services::payment::{most_complete_needed_package, user_missing_paid_features};
use crate::{libs::api_response::api_error, services::payment::PaidFeature};
use actix_web::Error;
use actix_web::body::BoxBody;
use actix_web::dev::ServiceResponse;
use actix_web::web;
use actix_web::{HttpMessage, HttpResponse, dev::ServiceRequest, middleware::Next};
use diesel::prelude::*;
use chrono::Utc;
use serde_json::json;

#[derive(Debug)]
pub struct PaymentPayload {
    pub active_payments: Vec<PaymentResponse>,
}

pub fn paid_features_guard_middleware_factory(
    required_features: Vec<PaidFeature>,
) -> impl Fn(
    ServiceRequest,
    Next<BoxBody>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ServiceResponse<BoxBody>, Error>> + 'static>,
> + Clone {
    move |req: ServiceRequest, next: Next<BoxBody>| {
        let required_features = required_features.clone();
        Box::pin(async move {
            let user_id = {
                let extensions = req.extensions();

                let access_payload = extensions
                    .get::<AccessPayload>()
                    .expect("AccessPayload must exist to check paid features");

                access_payload.user_id
            };

            let app_context = req
                .app_data::<web::Data<AppContext>>()
                .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

            let active_payments = match database::block_on_db(&app_context.pool, move |conn| {
                payments::table
                    .filter(payments::user_id.eq(user_id))
                    .filter(payments::is_success.eq(true))
                    .filter(payments::expires_at.is_null().or(payments::expires_at.gt(Utc::now())))
                    .select(PaymentResponse::as_select())
                    .load::<PaymentResponse>(conn)
            })
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    log::error!("[paid_features_guard_middleware] DB/block error: {:?}", e);
                    return Err(actix_web::error::ErrorServiceUnavailable(
                        "Database temporarily unavailable",
                    ));
                }
            };

            let missing_features = user_missing_paid_features(&active_payments, &required_features);
            let most_complete_needed_package = most_complete_needed_package(&missing_features);

            if !missing_features.is_empty() {
                let body = api_error(
                    "You must purchase packages to get access to this feature",
                    json!({
                       "missing_features": missing_features,
                       "most_complete_needed_package": most_complete_needed_package,
                    }),
                );
                return Ok(req
                    .into_response(HttpResponse::Forbidden().json(body))
                    .map_into_boxed_body());
            }

            let payload = PaymentPayload {
                active_payments,
            };

            req.extensions_mut().insert(payload);
            let res = next.call(req).await?;
            Ok(res.map_into_boxed_body())
        })
    }
}
