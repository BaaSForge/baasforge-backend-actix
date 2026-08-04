use crate::configs::Settings;
use crate::models::packages::Package;
use crate::models::payments::BillingPeriod;
use crate::schema::packages;
use crate::services::balance::record_top_up;
use crate::services::payment::grant_package_payment;
use chrono::Utc;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Text};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use stripe::{
    CheckoutSession, CheckoutSessionMode, Client, CreateCheckoutSession,
    CreateCheckoutSessionLineItems, CreateCheckoutSessionLineItemsPriceData,
    CreateCheckoutSessionLineItemsPriceDataProductData, Currency, StripeError,
};
use uuid::Uuid;

pub const CHECKOUT_KIND_PACKAGE: &str = "package";
pub const CHECKOUT_KIND_TOP_UP: &str = "top_up";

/// Minimal checkout session fields used for fulfillment.
/// Avoids deserializing the full Stripe `CheckoutSession` (breaks on newer API fields like `ui_mode=hosted_page`).
#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutFulfillmentSession {
    pub id: String,
    pub payment_status: String,
    pub metadata: Option<HashMap<String, String>>,
}

impl CheckoutFulfillmentSession {
    pub fn is_payable(&self) -> bool {
        self.payment_status == "paid" || self.payment_status == "no_payment_required"
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.as_ref().and_then(|m| m.get(key)).map(String::as_str)
    }
}

#[derive(Debug, Deserialize)]
struct StripeWebhookEventEnvelope {
    id: String,
    #[serde(rename = "type")]
    type_: String,
    data: StripeWebhookEventData,
}

#[derive(Debug, Deserialize)]
struct StripeWebhookEventData {
    object: serde_json::Value,
}

pub fn parse_checkout_fulfillment_event(
    payload: &str,
) -> Result<(String, String, CheckoutFulfillmentSession), String> {
    let envelope: StripeWebhookEventEnvelope =
        serde_json::from_str(payload).map_err(|e| format!("event envelope: {e}"))?;
    let session: CheckoutFulfillmentSession = serde_json::from_value(envelope.data.object)
        .map_err(|e| format!("checkout session object: {e}"))?;
    Ok((envelope.id, envelope.type_, session))
}

pub fn amount_to_unit_cents(amount: f64) -> i64 {
    (amount * 100.0).round() as i64
}

pub async fn create_package_checkout_session(
    settings: &Settings,
    user_id: Uuid,
    package: &Package,
    billing_period: BillingPeriod,
    amount: f64,
) -> Result<CheckoutSession, StripeError> {
    let user_id_str = user_id.to_string();
    let package_id_str = package.id.to_string();
    let billing_period_str = billing_period_to_str(billing_period).to_string();
    let product_name = format!("{} ({})", package.title, billing_period_str);

    let mut metadata = HashMap::new();
    metadata.insert("kind".to_string(), CHECKOUT_KIND_PACKAGE.to_string());
    metadata.insert("user_id".to_string(), user_id_str.clone());
    metadata.insert("package_id".to_string(), package_id_str);
    metadata.insert("billing_period".to_string(), billing_period_str);

    create_checkout_session(
        settings,
        &user_id_str,
        metadata,
        amount,
        product_name,
        package.description.clone(),
    )
    .await
}

pub async fn create_top_up_checkout_session(
    settings: &Settings,
    user_id: Uuid,
    amount: f64,
) -> Result<CheckoutSession, StripeError> {
    let user_id_str = user_id.to_string();
    let mut metadata = HashMap::new();
    metadata.insert("kind".to_string(), CHECKOUT_KIND_TOP_UP.to_string());
    metadata.insert("user_id".to_string(), user_id_str.clone());
    metadata.insert("amount".to_string(), format!("{amount:.2}"));

    create_checkout_session(
        settings,
        &user_id_str,
        metadata,
        amount,
        "Account top up".to_string(),
        Some("Add funds to your BaaS Forge balance".to_string()),
    )
    .await
}

async fn create_checkout_session(
    settings: &Settings,
    client_reference_id: &str,
    metadata: HashMap<String, String>,
    amount: f64,
    product_name: String,
    product_description: Option<String>,
) -> Result<CheckoutSession, StripeError> {
    let unit_amount = amount_to_unit_cents(amount);
    let currency = Currency::from_str(&settings.stripe_currency).map_err(|_| {
        StripeError::ClientError(format!(
            "Invalid STRIPE_CURRENCY: {}",
            settings.stripe_currency
        ))
    })?;

    let mut params = CreateCheckoutSession::new();
    params.mode = Some(CheckoutSessionMode::Payment);
    params.success_url = Some(settings.stripe_success_url.as_str());
    params.cancel_url = Some(settings.stripe_cancel_url.as_str());
    params.client_reference_id = Some(client_reference_id);
    params.metadata = Some(metadata);
    params.line_items = Some(vec![CreateCheckoutSessionLineItems {
        quantity: Some(1),
        price_data: Some(CreateCheckoutSessionLineItemsPriceData {
            currency,
            unit_amount: Some(unit_amount),
            product_data: Some(CreateCheckoutSessionLineItemsPriceDataProductData {
                name: product_name,
                description: product_description,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }]);

    let client = Client::new(settings.stripe_secret_key.clone());
    CheckoutSession::create(&client, params).await
}

#[derive(Debug)]
pub enum FulfillCheckoutError {
    MissingMetadata(&'static str),
    InvalidMetadata(&'static str),
    PackageNotFound,
    Db(diesel::result::Error),
}

impl std::fmt::Display for FulfillCheckoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FulfillCheckoutError::MissingMetadata(k) => write!(f, "missing metadata: {k}"),
            FulfillCheckoutError::InvalidMetadata(k) => write!(f, "invalid metadata: {k}"),
            FulfillCheckoutError::PackageNotFound => write!(f, "package not found or inactive"),
            FulfillCheckoutError::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

#[derive(Debug, QueryableByName)]
struct SessionExistsRow {
    #[diesel(sql_type = Bool)]
    exists: bool,
}

pub fn fulfill_checkout_session<C>(
    conn: &mut C,
    session: &CheckoutFulfillmentSession,
) -> Result<bool, FulfillCheckoutError>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    if !session.is_payable() {
        return Ok(false);
    }

    let session_id = session.id.as_str();
    let kind = session.meta("kind").unwrap_or(CHECKOUT_KIND_PACKAGE);

    match kind {
        CHECKOUT_KIND_TOP_UP => fulfill_top_up(conn, session, session_id),
        CHECKOUT_KIND_PACKAGE => fulfill_package(conn, session, session_id),
        _ => Err(FulfillCheckoutError::InvalidMetadata("kind")),
    }
}

fn fulfill_package<C>(
    conn: &mut C,
    session: &CheckoutFulfillmentSession,
    session_id: &str,
) -> Result<bool, FulfillCheckoutError>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    if session_already_recorded(conn, "payments", session_id)? {
        return Ok(false);
    }

    let user_id = session
        .meta("user_id")
        .ok_or(FulfillCheckoutError::MissingMetadata("user_id"))
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| FulfillCheckoutError::InvalidMetadata("user_id"))
        })?;
    let package_id = session
        .meta("package_id")
        .ok_or(FulfillCheckoutError::MissingMetadata("package_id"))
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| FulfillCheckoutError::InvalidMetadata("package_id"))
        })?;
    let billing_period = session
        .meta("billing_period")
        .ok_or(FulfillCheckoutError::MissingMetadata("billing_period"))
        .and_then(|s| {
            billing_period_from_str(s)
                .ok_or(FulfillCheckoutError::InvalidMetadata("billing_period"))
        })?;

    let package: Package = packages::table
        .filter(packages::id.eq(package_id))
        .filter(packages::is_active.eq(true))
        .select(Package::as_select())
        .first::<Package>(conn)
        .map_err(|e| match e {
            diesel::result::Error::NotFound => FulfillCheckoutError::PackageNotFound,
            other => FulfillCheckoutError::Db(other),
        })?;

    let gateway_metadata = json!({
        "provider": "stripe",
        "checkout_session_id": session_id,
        "kind": CHECKOUT_KIND_PACKAGE,
    });
    grant_package_payment(conn, user_id, &package, billing_period, Some(gateway_metadata))
        .map_err(FulfillCheckoutError::Db)?;
    Ok(true)
}

fn fulfill_top_up<C>(
    conn: &mut C,
    session: &CheckoutFulfillmentSession,
    session_id: &str,
) -> Result<bool, FulfillCheckoutError>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    if session_already_recorded(conn, "top_ups", session_id)? {
        return Ok(false);
    }

    let user_id = session
        .meta("user_id")
        .ok_or(FulfillCheckoutError::MissingMetadata("user_id"))
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| FulfillCheckoutError::InvalidMetadata("user_id"))
        })?;
    let amount = session
        .meta("amount")
        .ok_or(FulfillCheckoutError::MissingMetadata("amount"))
        .and_then(|s| {
            s.parse::<f64>()
                .ok()
                .filter(|a| a.is_finite() && *a > 0.0)
                .ok_or(FulfillCheckoutError::InvalidMetadata("amount"))
        })?;

    let gateway_metadata = json!({
        "provider": "stripe",
        "checkout_session_id": session_id,
        "kind": CHECKOUT_KIND_TOP_UP,
    });
    record_top_up(conn, user_id, amount, Some(gateway_metadata)).map_err(FulfillCheckoutError::Db)?;
    Ok(true)
}

fn session_already_recorded<C>(
    conn: &mut C,
    table: &str,
    session_id: &str,
) -> Result<bool, FulfillCheckoutError>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let sql = format!(
        "SELECT EXISTS(
            SELECT 1 FROM {table}
            WHERE gateway_metadata->>'checkout_session_id' = $1
        ) AS exists"
    );
    let already: SessionExistsRow = diesel::sql_query(sql)
        .bind::<Text, _>(session_id)
        .get_result(conn)
        .map_err(FulfillCheckoutError::Db)?;
    Ok(already.exists)
}

/// Cancels a granted package payment for this Checkout Session if one exists (async failure race).
/// Returns `true` when a row was cancelled.
pub fn cancel_checkout_session_payment<C>(
    conn: &mut C,
    session: &CheckoutFulfillmentSession,
) -> Result<bool, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let session_id = session.id.as_str();
    let now = Utc::now();
    let updated = diesel::sql_query(
        "UPDATE payments
         SET cancelled_at = $1
         WHERE is_success = true
           AND cancelled_at IS NULL
           AND gateway_metadata->>'checkout_session_id' = $2",
    )
    .bind::<diesel::sql_types::Timestamptz, _>(now)
    .bind::<Text, _>(session_id)
    .execute(conn)?;
    Ok(updated > 0)
}

pub fn checkout_session_log_context(session: &CheckoutFulfillmentSession) -> String {
    let kind = session.meta("kind").unwrap_or(CHECKOUT_KIND_PACKAGE);
    let user_id = session.meta("user_id").unwrap_or("-");
    format!(
        "session_id={} kind={kind} user_id={user_id} payment_status={}",
        session.id, session.payment_status
    )
}

fn billing_period_to_str(period: BillingPeriod) -> &'static str {
    match period {
        BillingPeriod::Monthly => "monthly",
        BillingPeriod::Annual => "annual",
    }
}

fn billing_period_from_str(s: &str) -> Option<BillingPeriod> {
    match s {
        "monthly" => Some(BillingPeriod::Monthly),
        "annual" => Some(BillingPeriod::Annual),
        _ => None,
    }
}
