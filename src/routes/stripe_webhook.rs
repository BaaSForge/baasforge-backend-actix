use crate::database;
use crate::libs::api_response::api_error;
use crate::routes::AppContext;
use crate::services::stripe::{FulfillCheckoutError, fulfill_checkout_session};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::json;
use std::borrow::Borrow;
use stripe::{EventObject, EventType, Webhook};

pub async fn handle_stripe_webhook(
    context: web::Data<AppContext>,
    req: HttpRequest,
    payload: web::Bytes,
) -> HttpResponse {
    let payload_str = match std::str::from_utf8(payload.borrow()) {
        Ok(s) => s,
        Err(_) => {
            return HttpResponse::BadRequest()
                .json(api_error("Invalid webhook payload encoding", json!({})));
        }
    };
    let signature = match req.headers().get("Stripe-Signature").and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => {
            return HttpResponse::BadRequest()
                .json(api_error("Missing Stripe-Signature header", json!({})));
        }
    };

    let event = match Webhook::construct_event(
        payload_str,
        signature,
        &context.settings.stripe_webhook_secret,
    ) {
        Ok(event) => event,
        Err(e) => {
            log::warn!("[handle_stripe_webhook] signature verification failed: {}", e);
            return HttpResponse::BadRequest()
                .json(api_error("Invalid Stripe webhook signature", json!({})));
        }
    };

    match event.type_ {
        EventType::CheckoutSessionCompleted | EventType::CheckoutSessionAsyncPaymentSucceeded => {
            let EventObject::CheckoutSession(session) = event.data.object else {
                log::error!(
                    "[handle_stripe_webhook] unexpected event object for {:?}",
                    event.type_
                );
                return HttpResponse::Ok().finish();
            };

            enum FulfillOut {
                Granted,
                AlreadyDone,
                Fail(FulfillCheckoutError),
            }
            let result = database::block_on_db(&context.pool, move |conn| {
                match fulfill_checkout_session(conn, &session) {
                    Ok(true) => Ok(FulfillOut::Granted),
                    Ok(false) => Ok(FulfillOut::AlreadyDone),
                    Err(e) => Ok(FulfillOut::Fail(e)),
                }
            })
            .await;

            match result {
                Ok(FulfillOut::Granted) => {
                    log::info!("[handle_stripe_webhook] fulfilled checkout session");
                }
                Ok(FulfillOut::AlreadyDone) => {
                    log::info!(
                        "[handle_stripe_webhook] checkout session skipped (unpaid or already fulfilled)"
                    );
                }
                Ok(FulfillOut::Fail(e)) => {
                    log::error!("[handle_stripe_webhook] fulfill failed: {}", e);
                    return HttpResponse::InternalServerError()
                        .json(api_error("Failed to fulfill checkout session", json!({})));
                }
                Err(e) => {
                    log::error!("[handle_stripe_webhook] DB/block error: {}", e);
                    return HttpResponse::ServiceUnavailable()
                        .json(api_error("Database temporarily unavailable", json!({})));
                }
            }
        }
        _ => {}
    }

    HttpResponse::Ok().finish()
}
