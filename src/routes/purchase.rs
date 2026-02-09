use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::{PaymentInsert};
use crate::schema::packages;
use crate::schema::payments;
use crate::{routes::AppContext};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use serde::Deserialize;

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

    let mut conn = context.pool.get().expect("DB conn");

    let package = packages::table
        .filter(packages::id.eq(body.package_id))
        .filter(packages::is_active.eq(true))
        .select(Package::as_select())
        .first::<Package>(&mut conn)
        .unwrap();

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
        expires_at: Some(Utc::now() + Duration::days(30)),
    };

    diesel::insert_into(payments::table)
        .values(&payment)
        .execute(&mut conn)
        .unwrap();

    HttpResponse::Created().finish()
}
