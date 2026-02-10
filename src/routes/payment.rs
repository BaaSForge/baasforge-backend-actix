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

    let mut conn = context.pool.get().expect("DB conn");

    let (payments_with_packages, largest_tier) =
        match get_user_active_paid_packages(&mut conn, access_payload.user_id) {
            Ok(t) => t,
            Err(e) => {
                return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
            }
        };

    let previous_successful_payment_count = match payments::table
        .filter(payments::user_id.eq(access_payload.user_id))
        .filter(payments::is_success.eq(true))
        .count()
        .get_result::<i64>(&mut conn)
    {
        Ok(count) => count,
        Err(e) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
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

    let mut conn = context.pool.get().expect("DB conn");

    let payments = match payments::table
        .filter(payments::user_id.eq(access_payload.user_id))
        .select(PaymentResponse::as_select())
        .load::<PaymentResponse>(&mut conn)
    {
        Ok(p) => p,
        Err(e) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Payments fetched successfully",
        json!(payments),
    ))
}
