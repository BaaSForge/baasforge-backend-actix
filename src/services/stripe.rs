use crate::configs::Settings;
use crate::models::packages::Package;
use crate::models::payments::BillingPeriod;
use crate::schema::packages;
use crate::services::payment::grant_package_payment;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Text};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use stripe::{
    CheckoutSession, CheckoutSessionMode, CheckoutSessionPaymentStatus, Client,
    CreateCheckoutSession, CreateCheckoutSessionLineItems,
    CreateCheckoutSessionLineItemsPriceData, CreateCheckoutSessionLineItemsPriceDataProductData,
    Currency, StripeError,
};
use uuid::Uuid;

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
    let unit_amount = amount_to_unit_cents(amount);
    let currency = Currency::from_str(&settings.stripe_currency).map_err(|_| {
        StripeError::ClientError(format!(
            "Invalid STRIPE_CURRENCY: {}",
            settings.stripe_currency
        ))
    })?;

    let user_id_str = user_id.to_string();
    let package_id_str = package.id.to_string();
    let billing_period_str = billing_period_to_str(billing_period).to_string();
    let product_name = format!("{} ({})", package.title, billing_period_str);

    let mut metadata = HashMap::new();
    metadata.insert("user_id".to_string(), user_id_str.clone());
    metadata.insert("package_id".to_string(), package_id_str);
    metadata.insert("billing_period".to_string(), billing_period_str);

    let mut params = CreateCheckoutSession::new();
    params.mode = Some(CheckoutSessionMode::Payment);
    params.success_url = Some(settings.stripe_success_url.as_str());
    params.cancel_url = Some(settings.stripe_cancel_url.as_str());
    params.client_reference_id = Some(user_id_str.as_str());
    params.metadata = Some(metadata);
    params.line_items = Some(vec![CreateCheckoutSessionLineItems {
        quantity: Some(1),
        price_data: Some(CreateCheckoutSessionLineItemsPriceData {
            currency,
            unit_amount: Some(unit_amount),
            product_data: Some(CreateCheckoutSessionLineItemsPriceDataProductData {
                name: product_name,
                description: package.description.clone(),
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
    session: &CheckoutSession,
) -> Result<bool, FulfillCheckoutError>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    if session.payment_status == CheckoutSessionPaymentStatus::Unpaid {
        return Ok(false);
    }

    let session_id = session.id.to_string();
    let already: SessionExistsRow = diesel::sql_query(
        "SELECT EXISTS(
            SELECT 1 FROM payments
            WHERE gateway_metadata->>'checkout_session_id' = $1
        ) AS exists",
    )
    .bind::<Text, _>(&session_id)
    .get_result(conn)
    .map_err(FulfillCheckoutError::Db)?;
    if already.exists {
        return Ok(false);
    }

    let metadata = session.metadata.as_ref();
    let user_id = metadata
        .and_then(|m| m.get("user_id"))
        .ok_or(FulfillCheckoutError::MissingMetadata("user_id"))
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| FulfillCheckoutError::InvalidMetadata("user_id"))
        })?;
    let package_id = metadata
        .and_then(|m| m.get("package_id"))
        .ok_or(FulfillCheckoutError::MissingMetadata("package_id"))
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| FulfillCheckoutError::InvalidMetadata("package_id"))
        })?;
    let billing_period = metadata
        .and_then(|m| m.get("billing_period"))
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
    });
    grant_package_payment(conn, user_id, &package, billing_period, Some(gateway_metadata))
        .map_err(FulfillCheckoutError::Db)?;
    Ok(true)
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
