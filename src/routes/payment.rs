use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::PaymentResponse;
use crate::schema::{packages, payments};
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::Utc;
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
    let now = Utc::now();

    let not_expired = payments::expires_at
        .is_null()
        .or(payments::expires_at.gt(now));

    let paid_packages: Vec<Package> = match payments::table
        .inner_join(packages::table.on(payments::package_id.eq(packages::id)))
        .filter(payments::user_id.eq(access_payload.user_id))
        .filter(payments::is_success.eq(true))
        .filter(not_expired)
        .order(payments::id.desc())
        .select(Package::as_select())
        .load::<Package>(&mut conn)
    {
        Ok(p) => p,
        Err(e) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
    };

    let largest_tier: i32 = match payments::table
        .filter(payments::user_id.eq(access_payload.user_id))
        .filter(payments::is_success.eq(true))
        .filter(not_expired)
        .select(diesel::dsl::max(payments::package_tier))
        .get_result::<Option<i32>>(&mut conn)
    {
        Ok(Some(t)) => t,
        Ok(None) => 0,
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
