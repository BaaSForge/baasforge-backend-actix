use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::PaymentInsert;
use crate::schema::{packages, payments};
use crate::services::payment::get_user_active_paid_packages;
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct InitiatePaymentRequest {
    package_id: i64,
}

pub async fn handle_initiate_purchase(
    context: web::Data<AppContext>,
    req: HttpRequest,
    body: web::Json<InitiatePaymentRequest>,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to initiate payment");

    let mut conn = match context.pool.get() {
        Ok(c) => c,
        Err(e) => {
            log::error!("[handle_initiate_purchase] Failed to get DB connection: {:?}", e);
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    let (paid_packages, largest_tier) =
        match get_user_active_paid_packages(&mut conn, access_payload.user_id) {
            Ok(t) => t,
            Err(e) => {
                return HttpResponse::InternalServerError()
                    .json(api_error(&e.to_string(), json!({})));
            }
        };

    let package: Package = match packages::table
        .filter(packages::id.eq(body.package_id))
        .filter(packages::is_active.eq(true))
        .select(Package::as_select())
        .first::<Package>(&mut conn)
    {
        Ok(p) => p,
        Err(diesel::result::Error::NotFound) => {
            return HttpResponse::BadRequest().json(api_error(
                "Package not found or inactive",
                json!({ "package_id": body.package_id }),
            ));
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
    };

    if package.tier <= largest_tier {
        return HttpResponse::BadRequest().json(api_error(
            "Target package tier must be larger than your current largest tier",
            json!({
                "package_tier": package.tier,
                "your_largest_tier": largest_tier,
            }),
        ));
    }

    let now = Utc::now();
    let new_expires_at = now + Duration::days(30);

    if !paid_packages.is_empty() {
        // TODO: refund remaining price for the previous active package(s) before upgrading
        let not_expired = payments::expires_at
            .is_null()
            .or(payments::expires_at.gt(now));
        match diesel::update(
            payments::table
                .filter(payments::user_id.eq(access_payload.user_id))
                .filter(payments::is_success.eq(true))
                .filter(not_expired),
        )
        .set(payments::expires_at.eq(now))
        .execute(&mut conn)
        {
            Ok(_) => {}
            Err(e) => {
                return HttpResponse::InternalServerError()
                    .json(api_error(&e.to_string(), json!({})));
            }
        }
    }

    let payment = PaymentInsert {
        user_id: access_payload.user_id,
        amount: package.price,
        package_id: package.id,
        package_title: Some(package.title.clone()),
        package_slug: Some(package.slug.clone()),
        package_tier: package.tier,
        package_price: Some(package.price),
        package_discount: package.discount_percent,
        package_is_tiral: package.is_trial.unwrap_or(false),
        is_success: true,
        gateway_metadata: None,
        expires_at: Some(new_expires_at),
    };

    match diesel::insert_into(payments::table)
        .values(&payment)
        .execute(&mut conn)
    {
        Ok(_) => HttpResponse::Created().json(api_success(
            "Purchase initiated successfully",
            json!({ "package_id": package.id, "expires_at": new_expires_at }),
        )),
        Err(e) => HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({}))),
    }
}
