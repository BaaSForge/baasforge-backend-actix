use crate::models::payments::{BillingPeriod, PaymentResponse};
use crate::models::top_ups::{TopUpInsert, TopUpResponse};
use crate::models::withdrawals::{WithdrawalInsert, WithdrawalResponse};
use crate::schema::{payments, projects, top_ups, users, withdrawals};
use crate::services::payment::get_user_active_paid_packages;
use crate::services::usage::{
    calculate_user_overall_usage_cost, calculate_user_usage_cost_between,
};
use chrono::Utc;
use diesel::dsl::sum;
use diesel::prelude::*;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct UserBalance {
    pub package_deposits: f64,
    pub top_ups: f64,
    pub deposited: f64,
    pub withdrawn: f64,
    pub total_usage: f64,
    pub balance: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionReserve {
    pub min_balance_to_keep: f64,
    pub max_withdraw_without_cancel: f64,
    pub balance: f64,
    pub effective_package_price: f64,
    pub period_usage_cost: f64,
}

pub fn set_billing_frozen<C>(
    conn: &mut C,
    user_id: Uuid,
    frozen: bool,
) -> Result<(), diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    diesel::update(users::table.filter(users::id.eq(user_id)))
        .set(users::is_billing_frozen.eq(frozen))
        .execute(conn)?;
    diesel::update(projects::table.filter(projects::owner_user_id.eq(user_id)))
        .set(projects::is_billing_frozen.eq(frozen))
        .execute(conn)?;
    Ok(())
}

pub fn unfreeze_user_billing<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<(), diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    set_billing_frozen(conn, user_id, false)
}

pub fn record_top_up<C>(
    conn: &mut C,
    user_id: Uuid,
    amount: f64,
    gateway_metadata: Option<Value>,
) -> Result<TopUpResponse, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let top_up = diesel::insert_into(top_ups::table)
        .values(TopUpInsert {
            user_id,
            amount,
            gateway_metadata,
        })
        .returning(TopUpResponse::as_select())
        .get_result(conn)?;
    unfreeze_user_billing(conn, user_id)?;
    Ok(top_up)
}

pub fn record_withdrawal<C>(
    conn: &mut C,
    user_id: Uuid,
    amount: f64,
    gateway_metadata: Option<Value>,
) -> Result<WithdrawalResponse, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    diesel::insert_into(withdrawals::table)
        .values(WithdrawalInsert {
            user_id,
            amount,
            gateway_metadata,
        })
        .returning(WithdrawalResponse::as_select())
        .get_result(conn)
}

// TODO: add periodic snapshots (e.g. daily) for faster balance reads, covering:
// - costs / usages (see also billing period totals)
// - withdrawals
// - package deposits (payments)
// - top ups
// Then derive balance from latest snapshot + deltas since that snapshot.
pub fn get_user_balance<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<UserBalance, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let package_deposits: f64 = payments::table
        .filter(payments::user_id.eq(user_id))
        .filter(payments::is_success.eq(true))
        .select(sum(payments::amount))
        .get_result::<Option<f64>>(conn)?
        .unwrap_or(0.0);

    let top_ups_total: f64 = top_ups::table
        .filter(top_ups::user_id.eq(user_id))
        .select(sum(top_ups::amount))
        .get_result::<Option<f64>>(conn)?
        .unwrap_or(0.0);

    let withdrawn: f64 = withdrawals::table
        .filter(withdrawals::user_id.eq(user_id))
        .select(sum(withdrawals::amount))
        .get_result::<Option<f64>>(conn)?
        .unwrap_or(0.0);

    let deposited = package_deposits + top_ups_total;
    let total_usage = calculate_user_overall_usage_cost(conn, user_id)?;

    Ok(UserBalance {
        package_deposits,
        top_ups: top_ups_total,
        deposited,
        withdrawn,
        total_usage,
        balance: deposited - withdrawn - total_usage,
    })
}

fn effective_package_price(payment: &PaymentResponse) -> f64 {
    match payment.billing_period {
        BillingPeriod::Annual => payment.amount,
        BillingPeriod::Monthly => {
            let base = payment.package_price.unwrap_or(payment.amount);
            let discount = payment.package_discount.unwrap_or(0).clamp(0, 100) as f64 / 100.0;
            base * (1.0 - discount)
        }
    }
}

pub fn get_subscription_reserve<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<SubscriptionReserve, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let balance = get_user_balance(conn, user_id)?;
    let (active, _) = get_user_active_paid_packages(conn, user_id)?;

    let Some((payment, _)) = active
        .into_iter()
        .max_by_key(|(payment, _)| payment.package_tier)
    else {
        return Ok(SubscriptionReserve {
            min_balance_to_keep: 0.0,
            max_withdraw_without_cancel: balance.balance.max(0.0),
            balance: balance.balance,
            effective_package_price: 0.0,
            period_usage_cost: 0.0,
        });
    };

    let effective_package_price = effective_package_price(&payment);
    let period_usage_cost =
        calculate_user_usage_cost_between(conn, user_id, payment.created_at, Utc::now())?;
    let min_balance_to_keep = (effective_package_price - period_usage_cost).max(0.0);
    let max_withdraw_without_cancel = (balance.balance - min_balance_to_keep).max(0.0);

    Ok(SubscriptionReserve {
        min_balance_to_keep,
        max_withdraw_without_cancel,
        balance: balance.balance,
        effective_package_price,
        period_usage_cost,
    })
}

pub fn cancel_active_subscriptions<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<usize, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let now = Utc::now();
    let not_expired = payments::expires_at
        .is_null()
        .or(payments::expires_at.gt(now));

    diesel::update(
        payments::table
            .filter(payments::user_id.eq(user_id))
            .filter(payments::is_success.eq(true))
            .filter(payments::cancelled_at.is_null())
            .filter(not_expired),
    )
    .set(payments::cancelled_at.eq(now))
    .execute(conn)
}
