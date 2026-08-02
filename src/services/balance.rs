use crate::models::top_ups::{TopUpInsert, TopUpResponse};
use crate::models::withdrawals::{WithdrawalInsert, WithdrawalResponse};
use crate::schema::{payments, top_ups, withdrawals};
use crate::services::usage::calculate_user_overall_usage_cost;
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
    diesel::insert_into(top_ups::table)
        .values(TopUpInsert {
            user_id,
            amount,
            gateway_metadata,
        })
        .returning(TopUpResponse::as_select())
        .get_result(conn)
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
