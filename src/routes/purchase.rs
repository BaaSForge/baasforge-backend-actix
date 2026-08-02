use crate::database;
use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::{BillingPeriod, PaymentInsert};
use crate::schema::{packages, payments};
use crate::services::balance::unfreeze_user_billing;
use crate::services::payment::get_user_active_paid_packages;
use crate::services::pricing::config::ANNUAL_BILLING_DISCOUNT;
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct InitiatePaymentRequest {
    package_id: Uuid,
    #[serde(default)]
    billing_period: Option<BillingPeriod>,
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

    let user_id = access_payload.user_id;
    let package_id = body.package_id;
    let billing_period = body.billing_period.unwrap_or_default();
    let is_annual = billing_period == BillingPeriod::Annual;

    enum PurchaseOut {
        PackageNotFound,
        TierNotLarger,
        Ok {
            package: Package,
            new_expires_at: chrono::DateTime<Utc>,
            amount: f64,
        },
        #[allow(dead_code)]
        DbErr(diesel::result::Error),
    }
    let result = database::block_on_db(&context.pool, move |conn| {
        let (paid_packages, largest_tier) = get_user_active_paid_packages(conn, user_id)?;
        let package: Package = match packages::table
            .filter(packages::id.eq(package_id))
            .filter(packages::is_active.eq(true))
            .select(Package::as_select())
            .first::<Package>(conn)
        {
            Ok(p) => p,
            Err(diesel::result::Error::NotFound) => return Ok(PurchaseOut::PackageNotFound),
            Err(e) => return Err(e),
        };
        if package.tier <= largest_tier {
            return Ok(PurchaseOut::TierNotLarger);
        }
        let now = Utc::now();
        let (new_expires_at, amount) = if is_annual {
            let annual_amount = package.price * 12.0 * (1.0 - ANNUAL_BILLING_DISCOUNT);
            (now + Duration::days(365), annual_amount)
        } else {
            (now + Duration::days(30), package.price)
        };
        if !paid_packages.is_empty() {
            // TODO: refund remaining price for the previous active package(s) before upgrading
            let not_expired = payments::expires_at
                .is_null()
                .or(payments::expires_at.gt(now));
            diesel::update(
                payments::table
                    .filter(payments::user_id.eq(user_id))
                    .filter(payments::is_success.eq(true))
                    .filter(payments::cancelled_at.is_null())
                    .filter(not_expired),
            )
            .set(payments::expires_at.eq(now))
            .execute(conn)?;
        }
        let payment = PaymentInsert {
            user_id,
            amount,
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
            billing_period,
        };
        diesel::insert_into(payments::table)
            .values(&payment)
            .execute(conn)?;
        unfreeze_user_billing(conn, user_id)?;
        Ok(PurchaseOut::Ok { package, new_expires_at, amount })
    })
    .await;

    match result {
        Ok(PurchaseOut::PackageNotFound) => HttpResponse::BadRequest().json(api_error(
            "Package not found or inactive",
            json!({ "package_id": package_id }),
        )),
        Ok(PurchaseOut::TierNotLarger) => HttpResponse::BadRequest().json(api_error(
            "Target package tier must be larger than your current largest tier",
            json!({}),
        )),
        Ok(PurchaseOut::Ok { package, new_expires_at, amount }) => {
            HttpResponse::Created().json(api_success(
                "Purchase initiated successfully",
                json!({
                    "package_id": package.id,
                    "expires_at": new_expires_at,
                    "amount": amount,
                    "billing_period": billing_period,
                }),
            ))
        }
        Ok(PurchaseOut::DbErr(e)) => {
            HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})))
        }
        Err(database::BlockingDbError::Query(e)) => {
            HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})))
        }
        Err(e) => {
            log::error!("[handle_initiate_purchase] DB/block error: {}", e);
            HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})))
        }
    }
}
