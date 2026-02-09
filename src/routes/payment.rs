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
pub struct PaidPackageResponse {
    pub paid_packages: Vec<Package>,
    pub largest_tier: i32,
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

    let (paid_packages, largest_tier) = match get_user_active_paid_packages(&mut conn, access_payload.user_id) {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
    };

    let body = PaidPackageResponse {
        paid_packages,
        largest_tier,
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
