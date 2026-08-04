use crate::database;
use crate::libs::api_response::api_error;
use crate::routes::AppContext;
use crate::services::stripe::{
    FulfillCheckoutError, cancel_checkout_session_payment, checkout_session_log_context,
    fulfill_checkout_session,
};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::json;
use std::borrow::Borrow;
use stripe::{CheckoutSession, EventObject, EventType, Webhook};

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
            let Some(session) = checkout_session_from_event(&event.data.object, &event.type_) else {
                return HttpResponse::Ok().finish();
            };
            return handle_fulfill(context, session).await;
        }
        EventType::CheckoutSessionAsyncPaymentFailed => {
            let Some(session) = checkout_session_from_event(&event.data.object, &event.type_) else {
                return HttpResponse::Ok().finish();
            };
            return handle_async_payment_failed(context, session).await;
        }
        EventType::CheckoutSessionExpired => {
            let Some(session) = checkout_session_from_event(&event.data.object, &event.type_) else {
                return HttpResponse::Ok().finish();
            };
            log::info!(
                "[handle_stripe_webhook] checkout session expired ({})",
                checkout_session_log_context(&session)
            );
        }
        _ => {}
    }

    HttpResponse::Ok().finish()
}

fn checkout_session_from_event(
    object: &EventObject,
    event_type: &EventType,
) -> Option<CheckoutSession> {
    match object {
        EventObject::CheckoutSession(session) => Some(session.clone()),
        _ => {
            log::error!(
                "[handle_stripe_webhook] unexpected event object for {:?}",
                event_type
            );
            None
        }
    }
}

async fn handle_fulfill(
    context: web::Data<AppContext>,
    session: CheckoutSession,
) -> HttpResponse {
    enum FulfillOut {
        Granted,
        AlreadyDone,
        Fail(FulfillCheckoutError),
    }
    let ctx = checkout_session_log_context(&session);
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
            log::info!("[handle_stripe_webhook] fulfilled checkout session ({ctx})");
        }
        Ok(FulfillOut::AlreadyDone) => {
            log::info!(
                "[handle_stripe_webhook] checkout session skipped unpaid/already fulfilled ({ctx})"
            );
        }
        Ok(FulfillOut::Fail(e)) => {
            log::error!("[handle_stripe_webhook] fulfill failed ({ctx}): {e}");
            return HttpResponse::InternalServerError()
                .json(api_error("Failed to fulfill checkout session", json!({})));
        }
        Err(e) => {
            log::error!("[handle_stripe_webhook] DB/block error ({ctx}): {e}");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    }

    HttpResponse::Ok().finish()
}

async fn handle_async_payment_failed(
    context: web::Data<AppContext>,
    session: CheckoutSession,
) -> HttpResponse {
    let ctx = checkout_session_log_context(&session);
    let result = database::block_on_db(&context.pool, move |conn| {
        cancel_checkout_session_payment(conn, &session)
    })
    .await;

    match result {
        Ok(true) => {
            log::warn!(
                "[handle_stripe_webhook] async payment failed; cancelled granted payment ({ctx})"
            );
        }
        Ok(false) => {
            log::info!(
                "[handle_stripe_webhook] async payment failed; no granted payment to cancel ({ctx})"
            );
        }
        Err(e) => {
            log::error!(
                "[handle_stripe_webhook] async payment failed handling error ({ctx}): {e}"
            );
            return HttpResponse::InternalServerError()
                .json(api_error("Failed to handle async payment failure", json!({})));
        }
    }

    HttpResponse::Ok().finish()
}
