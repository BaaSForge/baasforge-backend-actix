use crate::database;
use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::BillingPeriod;
use crate::schema::packages;
use crate::services::payment::{get_user_active_paid_packages, grant_package_payment};
use crate::services::pricing::package_subscription_amount;
use crate::services::stripe::{amount_to_unit_cents, create_package_checkout_session};
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
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

    enum PurchasePrep {
        PackageNotFound,
        TierNotLarger,
        Ok { package: Package, amount: f64 },
    }
    let prep = database::block_on_db(&context.pool, move |conn| {
        let (_, largest_tier) = get_user_active_paid_packages(conn, user_id)?;
        let package: Package = match packages::table
            .filter(packages::id.eq(package_id))
            .filter(packages::is_active.eq(true))
            .select(Package::as_select())
            .first::<Package>(conn)
        {
            Ok(p) => p,
            Err(diesel::result::Error::NotFound) => return Ok(PurchasePrep::PackageNotFound),
            Err(e) => return Err(e),
        };
        if package.tier <= largest_tier {
            return Ok(PurchasePrep::TierNotLarger);
        }
        let amount =
            package_subscription_amount(package.price, package.discount_percent, is_annual);
        Ok(PurchasePrep::Ok { package, amount })
    })
    .await;

    let (package, amount) = match prep {
        Ok(PurchasePrep::PackageNotFound) => {
            return HttpResponse::BadRequest().json(api_error(
                "Package not found or inactive",
                json!({ "package_id": package_id }),
            ));
        }
        Ok(PurchasePrep::TierNotLarger) => {
            return HttpResponse::BadRequest().json(api_error(
                "Target package tier must be larger than your current largest tier",
                json!({}),
            ));
        }
        Ok(PurchasePrep::Ok { package, amount }) => (package, amount),
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError()
                .json(api_error(&e.to_string(), json!({})));
        }
        Err(e) => {
            log::error!("[handle_initiate_purchase] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    if amount_to_unit_cents(amount) <= 0 {
        let package_for_grant = package.clone();
        let grant = database::block_on_db(&context.pool, move |conn| {
            grant_package_payment(conn, user_id, &package_for_grant, billing_period, None)
        })
        .await;

        return match grant {
            Ok(granted) => HttpResponse::Created().json(api_success(
                "Purchase completed successfully",
                json!({
                    "package_id": granted.package.id,
                    "expires_at": granted.expires_at,
                    "amount": granted.amount,
                    "billing_period": granted.billing_period,
                    "checkout_url": null,
                    "session_id": null,
                }),
            )),
            Err(database::BlockingDbError::Query(e)) => {
                HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})))
            }
            Err(e) => {
                log::error!("[handle_initiate_purchase] zero-amount grant error: {}", e);
                HttpResponse::ServiceUnavailable()
                    .json(api_error("Database temporarily unavailable", json!({})))
            }
        };
    }

    match create_package_checkout_session(
        &context.settings,
        user_id,
        &package,
        billing_period,
        amount,
    )
    .await
    {
        Ok(session) => {
            let checkout_url = session.url.clone();
            HttpResponse::Created().json(api_success(
                "Checkout session created",
                json!({
                    "package_id": package.id,
                    "amount": amount,
                    "billing_period": billing_period,
                    "checkout_url": checkout_url,
                    "session_id": session.id,
                }),
            ))
        }
        Err(e) => {
            log::error!("[handle_initiate_purchase] Stripe checkout error: {}", e);
            HttpResponse::BadGateway().json(api_error(
                "Failed to create Stripe checkout session",
                json!({ "detail": e.to_string() }),
            ))
        }
    }
}
