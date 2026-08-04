use super::config::ANNUAL_BILLING_DISCOUNT;

/// Amount charged for a package purchase (Stripe) and credited to the user's balance.
///
/// Package / annual discounts lower the **sale price**. We do not credit list price
/// while charging less — balance increases by exactly this amount (pay $X → get $X credit).
pub fn package_subscription_amount(
    list_monthly_price: f64,
    discount_percent: Option<i32>,
    is_annual: bool,
) -> f64 {
    let discount = discount_percent.unwrap_or(0).clamp(0, 100) as f64 / 100.0;
    let discounted_monthly = list_monthly_price * (1.0 - discount);
    if is_annual {
        discounted_monthly * 12.0 * (1.0 - ANNUAL_BILLING_DISCOUNT)
    } else {
        discounted_monthly
    }
}
