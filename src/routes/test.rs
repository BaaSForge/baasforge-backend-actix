use crate::libs::api_response::api_error;
use crate::models::server_clusters::ServerCluster;
use crate::schema::server_clusters;
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpResponse, web};
use diesel::prelude::*;
use serde_json::json;

pub async fn handle_get_first_server_cluster(
    context: web::Data<AppContext>,
) -> HttpResponse {
    let mut conn = context.pool.get().expect("DB conn");

    let server_cluster = server_clusters::table
        .select(ServerCluster::as_select())
        .first::<ServerCluster>(&mut conn)
        .optional()
        .expect("DB query failed");

    match server_cluster {
        Some(cluster) => {
            HttpResponse::Ok().json(api_success(
                "Server cluster fetched successfully",
                json!(cluster),
            ))
        }
        None => HttpResponse::NotFound().json(api_error(
            "No server cluster found",
            json!({}),
        )),
    }
}
