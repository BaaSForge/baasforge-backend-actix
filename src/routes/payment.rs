use crate::database;
use crate::middlewares::auth::AccessPayload;
use crate::models::packages::Package;
use crate::models::payments::PaymentResponse;
use crate::models::top_ups::TopUpResponse;
use crate::models::withdrawals::WithdrawalResponse;
use crate::schema::{payments, top_ups, withdrawals};
use crate::services::balance::{
    cancel_active_subscriptions, get_subscription_reserve, get_user_balance, record_top_up,
    record_withdrawal, set_billing_frozen,
};
use crate::services::payment::get_user_active_paid_packages;
use crate::{
    libs::api_response::{api_error, api_success},
    routes::AppContext,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize)]
pub struct PaymentWithPackage {
    pub payment: PaymentResponse,
    pub package: Package,
}

#[derive(Serialize)]
pub struct PaidPackageResponse {
    pub payments: Vec<PaymentWithPackage>,
    pub largest_tier: i32,
    pub can_purchase_trial: bool,
}

pub async fn handle_get_user_paid_package(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get paid package");

    let user_id = access_payload.user_id;

    let (payments_with_packages, largest_tier, previous_successful_payment_count) =
        match database::block_on_db(&context.pool, move |conn| {
            let (packages, largest_tier) = get_user_active_paid_packages(conn, user_id)?;
            let count = payments::table
                .filter(payments::user_id.eq(user_id))
                .filter(payments::is_success.eq(true))
                .count()
                .get_result::<i64>(conn)?;
            Ok::<_, diesel::result::Error>((packages, largest_tier, count))
        })
        .await
        {
            Ok(t) => t,
            Err(e) => {
                log::error!("[handle_get_user_paid_package] DB/block error: {}", e);
                return HttpResponse::ServiceUnavailable()
                    .json(api_error("Database temporarily unavailable", json!({})));
            }
        };

    let can_purchase_trial = previous_successful_payment_count == 0;

    let payments: Vec<PaymentWithPackage> = payments_with_packages
        .into_iter()
        .map(|(payment, package)| PaymentWithPackage { payment, package })
        .collect();

    let body = PaidPackageResponse {
        payments,
        largest_tier,
        can_purchase_trial,
    };

    HttpResponse::Ok().json(api_success(
        "Active package fetched successfully",
        json!(body),
    ))
}

#[derive(Deserialize)]
pub struct InitiateTopUpRequest {
    amount: f64,
}

pub async fn handle_initiate_top_up(
    context: web::Data<AppContext>,
    req: HttpRequest,
    body: web::Json<InitiateTopUpRequest>,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to initiate top up");

    let user_id = access_payload.user_id;
    let amount = body.amount;

    if !amount.is_finite() || amount < 5.0 {
        return HttpResponse::BadRequest().json(api_error(
            "Amount must be at least 5 USD",
            json!({ "amount": amount, "minimum": 5.0 }),
        ));
    }

    let top_up = match database::block_on_db(&context.pool, move |conn| {
        record_top_up(conn, user_id, amount, None)
    })
    .await
    {
        Ok(t) => t,
        Err(e) => {
            log::error!("[handle_initiate_top_up] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Created().json(api_success(
        "Top up initiated successfully",
        json!({
            "id": top_up.id,
            "amount": top_up.amount,
            "created_at": top_up.created_at,
        }),
    ))
}

#[derive(Deserialize)]
pub struct InitiateWithdrawalRequest {
    amount: f64,
}

pub async fn handle_initiate_withdrawal(
    context: web::Data<AppContext>,
    req: HttpRequest,
    body: web::Json<InitiateWithdrawalRequest>,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to initiate withdrawal");

    let user_id = access_payload.user_id;
    let amount = body.amount;

    if !amount.is_finite() || amount <= 0.0 {
        return HttpResponse::BadRequest().json(api_error(
            "Amount must be a positive number",
            json!({ "amount": amount }),
        ));
    }

    enum WithdrawOut {
        InsufficientBalance { balance: f64 },
        Ok {
            withdrawal: crate::models::withdrawals::WithdrawalResponse,
            subscription_cancelled: bool,
        },
    }

    let result = database::block_on_db(&context.pool, move |conn| {
        let balance = get_user_balance(conn, user_id)?;
        if amount > balance.balance {
            return Ok(WithdrawOut::InsufficientBalance {
                balance: balance.balance,
            });
        }
        let reserve = get_subscription_reserve(conn, user_id)?;
        let remaining = balance.balance - amount;
        let subscription_cancelled = if remaining < reserve.min_balance_to_keep {
            cancel_active_subscriptions(conn, user_id)?;
            true
        } else {
            false
        };
        let withdrawal = record_withdrawal(conn, user_id, amount, None)?;
        if remaining <= 0.0 {
            set_billing_frozen(conn, user_id, true)?;
        }
        Ok(WithdrawOut::Ok {
            withdrawal,
            subscription_cancelled,
        })
    })
    .await;

    match result {
        Ok(WithdrawOut::InsufficientBalance { balance }) => {
            HttpResponse::BadRequest().json(api_error(
                "Amount exceeds available balance",
                json!({ "amount": amount, "balance": balance }),
            ))
        }
        Ok(WithdrawOut::Ok {
            withdrawal,
            subscription_cancelled,
        }) => HttpResponse::Created().json(api_success(
            "Withdrawal initiated successfully",
            json!({
                "id": withdrawal.id,
                "amount": withdrawal.amount,
                "created_at": withdrawal.created_at,
                "subscription_cancelled": subscription_cancelled,
            }),
        )),
        Err(e) => {
            log::error!("[handle_initiate_withdrawal] DB/block error: {}", e);
            HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})))
        }
    }
}

pub async fn handle_get_subscription_reserve(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get subscription reserve");

    let user_id = access_payload.user_id;

    let reserve = match database::block_on_db(&context.pool, move |conn| {
        get_subscription_reserve(conn, user_id)
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            log::error!("[handle_get_subscription_reserve] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Subscription reserve fetched successfully",
        json!(reserve),
    ))
}

pub async fn handle_get_user_balance(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get balance");

    let user_id = access_payload.user_id;

    let balance = match database::block_on_db(&context.pool, move |conn| {
        get_user_balance(conn, user_id)
    })
    .await
    {
        Ok(b) => b,
        Err(e) => {
            log::error!("[handle_get_user_balance] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Balance fetched successfully",
        json!(balance),
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

    let user_id = access_payload.user_id;

    let payments = match database::block_on_db(&context.pool, move |conn| {
        payments::table
            .filter(payments::user_id.eq(user_id))
            .select(PaymentResponse::as_select())
            .load::<PaymentResponse>(conn)
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            log::error!("[handle_list_user_payments] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Payments fetched successfully",
        json!(payments),
    ))
}

pub async fn handle_list_user_top_ups(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list top ups");

    let user_id = access_payload.user_id;

    let items = match database::block_on_db(&context.pool, move |conn| {
        top_ups::table
            .filter(top_ups::user_id.eq(user_id))
            .order(top_ups::created_at.desc())
            .select(TopUpResponse::as_select())
            .load::<TopUpResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            log::error!("[handle_list_user_top_ups] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Top ups fetched successfully",
        json!(items),
    ))
}

pub async fn handle_list_user_withdrawals(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list withdrawals");

    let user_id = access_payload.user_id;

    let items = match database::block_on_db(&context.pool, move |conn| {
        withdrawals::table
            .filter(withdrawals::user_id.eq(user_id))
            .order(withdrawals::created_at.desc())
            .select(WithdrawalResponse::as_select())
            .load::<WithdrawalResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            log::error!("[handle_list_user_withdrawals] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Withdrawals fetched successfully",
        json!(items),
    ))
}
