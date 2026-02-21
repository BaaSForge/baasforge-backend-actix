use crate::models::packages::Package;
use crate::schema::packages;
use crate::{libs::api_response::{api_error, api_success}, routes::AppContext};
use actix_web::{HttpResponse, web};
use diesel::prelude::*;
use serde_json::json;

pub async fn handle_list_packages(context: web::Data<AppContext>) -> HttpResponse {
    let mut conn = match context.pool.get() {
        Ok(c) => c,
        Err(e) => {
            log::error!("[handle_list_packages] Failed to get DB connection: {:?}", e);
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    let packages = packages::table
        .filter(packages::is_active.eq(true))
        .order(packages::tier.asc())
        .select(Package::as_select())
        .load::<Package>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success("Packages fetched successfully", json!(packages)))
}
