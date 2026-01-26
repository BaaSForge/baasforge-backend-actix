use crate::models::packages::Package;
use crate::schema::packages;
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpResponse, web};
use diesel::prelude::*;
use serde_json::json;

pub async fn handle_list_packages(context: web::Data<AppContext>) -> HttpResponse {
    let mut conn = context.pool.get().expect("DB conn");

    let tokens = packages::table
        .filter(packages::is_active.eq(true))
        .select(Package::as_select())
        .load::<Package>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success("Packages fetched successfully", json!(tokens)))
}
