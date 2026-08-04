use crate::database;
use crate::libs::api_response::api_error;
use crate::routes::AppContext;
use crate::services::stripe::{
    CheckoutFulfillmentSession, FulfillCheckoutError, cancel_checkout_session_payment,
    checkout_session_log_context, fulfill_checkout_session, parse_checkout_fulfillment_event,
};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::json;
use std::borrow::Borrow;
use stripe::{Webhook, WebhookError};

pub async fn handle_stripe_webhook(
    context: web::Data<AppContext>,
    req: HttpRequest,
    payload: web::Bytes,
) -> HttpResponse {
    let payload_len = payload.len();
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let signature_header_raw = req.headers().get("Stripe-Signature").map(|v| v.as_bytes());

    let payload_str = match std::str::from_utf8(payload.borrow()) {
        Ok(s) => s,
        Err(e) => {
            log::warn!(
                "[handle_stripe_webhook] invalid payload encoding: {e}; payload_len={payload_len} content_type={content_type}"
            );
            return HttpResponse::BadRequest()
                .json(api_error("Invalid webhook payload encoding", json!({})));
        }
    };
    let signature = match req.headers().get("Stripe-Signature").and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => {
            log::warn!(
                "[handle_stripe_webhook] missing Stripe-Signature; payload_len={payload_len} content_type={content_type} header_present={}",
                signature_header_raw.is_some()
            );
            return HttpResponse::BadRequest()
                .json(api_error("Missing Stripe-Signature header", json!({})));
        }
    };

    let webhook_secret = context.settings.stripe_webhook_secret.as_str();
    log::info!(
        "[handle_stripe_webhook] verifying: payload_len={payload_len} content_type={content_type} {} signature={}",
        webhook_secret_log_context(webhook_secret),
        signature_log_context(signature)
    );

    // Signature is checked before full Event deserialize. BadParse still means the signature is valid
    // (async-stripe 0.41 cannot deserialize newer CheckoutSession fields like ui_mode=hosted_page).
    let full_event_ok = match Webhook::construct_event(payload_str, signature, webhook_secret) {
        Ok(event) => {
            log::info!(
                "[handle_stripe_webhook] signature ok: event_id={} event_type={:?}",
                event.id,
                event.type_
            );
            true
        }
        Err(WebhookError::BadParse(e)) => {
            log::warn!(
                "[handle_stripe_webhook] signature ok but full Event parse failed ({e}); using minimal checkout fields"
            );
            false
        }
        Err(e) => {
            log::warn!(
                "[handle_stripe_webhook] signature verification failed: {e}; payload_len={payload_len} content_type={content_type} {} signature={} hint=STRIPE_WEBHOOK_SECRET must match the whsec_ printed by `stripe listen` (not the Dashboard endpoint secret). Restart the API after updating it.",
                webhook_secret_log_context(webhook_secret),
                signature_log_context(signature)
            );
            return HttpResponse::BadRequest()
                .json(api_error("Invalid Stripe webhook signature", json!({})));
        }
    };

    let (event_id, event_type, session) = match parse_checkout_fulfillment_event(payload_str) {
        Ok(parsed) => parsed,
        Err(e) => {
            if full_event_ok {
                return HttpResponse::Ok().finish();
            }
            log::error!(
                "[handle_stripe_webhook] verified webhook but minimal checkout parse failed: {e}"
            );
            return HttpResponse::BadRequest().json(api_error(
                "Failed to parse checkout session webhook",
                json!({ "detail": e }),
            ));
        }
    };

    log::info!(
        "[handle_stripe_webhook] checkout event: event_id={event_id} event_type={event_type} ({})",
        checkout_session_log_context(&session)
    );

    match event_type.as_str() {
        "checkout.session.completed" | "checkout.session.async_payment_succeeded" => {
            handle_fulfill(context, session).await
        }
        "checkout.session.async_payment_failed" => {
            handle_async_payment_failed(context, session).await
        }
        "checkout.session.expired" => {
            log::info!(
                "[handle_stripe_webhook] checkout session expired ({})",
                checkout_session_log_context(&session)
            );
            HttpResponse::Ok().finish()
        }
        other => {
            log::info!("[handle_stripe_webhook] ignoring checkout-shaped event_type={other}");
            HttpResponse::Ok().finish()
        }
    }
}

async fn handle_fulfill(
    context: web::Data<AppContext>,
    session: CheckoutFulfillmentSession,
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
    session: CheckoutFulfillmentSession,
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

fn webhook_secret_log_context(secret: &str) -> String {
    let trimmed = secret.trim();
    let len = trimmed.len();
    let prefix_ok = trimmed.starts_with("whsec_");
    let head = trimmed.chars().take(10).collect::<String>();
    let tail = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("secret_len={len} secret_prefix_ok={prefix_ok} secret_head={head}… secret_tail=…{tail}")
}

fn signature_log_context(signature: &str) -> String {
    let len = signature.len();
    let t = signature
        .split(',')
        .find_map(|part| part.strip_prefix("t="))
        .unwrap_or("-");
    let v1_count = signature.split(',').filter(|p| p.starts_with("v1=")).count();
    format!("sig_len={len} t={t} v1_count={v1_count}")
}
