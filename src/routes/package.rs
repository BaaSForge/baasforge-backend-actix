use crate::database;
use crate::models::packages::Package;
use crate::schema::packages;
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpResponse, web};
use diesel::prelude::*;
use serde_json::json;

pub async fn handle_list_packages(context: web::Data<AppContext>) -> HttpResponse {
    let packages = match database::block_on_db(&context.pool, |conn| {
        packages::table
            .filter(packages::is_active.eq(true))
            .order(packages::tier.asc())
            .select(Package::as_select())
            .load::<Package>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_list_packages] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Packages fetched successfully",
        json!(packages),
    ))
}
