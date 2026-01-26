use crate::middlewares::auth::AccessPayload;
use crate::models::payments::PaymentResponse;
use crate::schema::payments;
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use serde_json::json;

pub async fn handle_list_user_payments(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list payments");

    let mut conn = context.pool.get().expect("DB conn");

    let payments = payments::table
        .filter(payments::user_id.eq(access_payload.user_id))
        .select(PaymentResponse::as_select())
        .load::<PaymentResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success("Payments fetched successfully", json!(payments)))
}
