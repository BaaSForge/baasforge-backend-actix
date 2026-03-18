use crate::database;
use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::PaymentResponse;
use crate::schema::payments;
use crate::services::payment::get_user_active_paid_packages;
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
pub struct PaymentWithPackage {
    pub payment: PaymentResponse,
    pub package: Package,
}

#[derive(Serialize)]
pub struct PaidPackageResponse {
    pub payments: Vec<PaymentWithPackage>,
    pub largest_tier: i32,
    pub can_purchase_trial: bool,
}

pub async fn handle_get_user_paid_package(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get paid package");

    let user_id = access_payload.user_id;

    let (payments_with_packages, largest_tier, previous_successful_payment_count) =
        match database::block_on_db(&context.pool, move |conn| {
            let (packages, largest_tier) = get_user_active_paid_packages(conn, user_id)?;
            let count = payments::table
                .filter(payments::user_id.eq(user_id))
                .filter(payments::is_success.eq(true))
                .count()
                .get_result::<i64>(conn)?;
            Ok::<_, diesel::result::Error>((packages, largest_tier, count))
        })
        .await
        {
            Ok(t) => t,
            Err(e) => {
                log::error!("[handle_get_user_paid_package] DB/block error: {}", e);
                return HttpResponse::ServiceUnavailable()
                    .json(api_error("Database temporarily unavailable", json!({})));
            }
        };

    let can_purchase_trial = previous_successful_payment_count == 0;

    let payments: Vec<PaymentWithPackage> = payments_with_packages
        .into_iter()
        .map(|(payment, package)| PaymentWithPackage { payment, package })
        .collect();

    let body = PaidPackageResponse {
        payments,
        largest_tier,
        can_purchase_trial,
    };

    HttpResponse::Ok().json(api_success(
        "Active package fetched successfully",
        json!(body),
    ))
}

pub async fn handle_list_user_payments(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list payments");

    let user_id = access_payload.user_id;

    let payments = match database::block_on_db(&context.pool, move |conn| {
        payments::table
            .filter(payments::user_id.eq(user_id))
            .select(PaymentResponse::as_select())
            .load::<PaymentResponse>(conn)
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            log::error!("[handle_list_user_payments] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Payments fetched successfully",
        json!(payments),
    ))
}
