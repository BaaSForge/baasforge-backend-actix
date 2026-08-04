use super::config::ANNUAL_BILLING_DISCOUNT;

/// Amount charged via Stripe for a package purchase.
///
/// Package / annual discounts lower the **sale price**. For paid packages, balance
/// credit matches this amount (pay $X → get $X). Free plans (sale `$0` with list
/// price > 0) still credit list price — see [`package_balance_credit_amount`].
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

/// Amount added to the user's balance for a package payment.
///
/// Free plans (100% off → `$0` charge, list price > 0) credit the list monthly
/// price as promotional balance. Paid packages credit the sale price.
pub fn package_balance_credit_amount(
    list_monthly_price: f64,
    discount_percent: Option<i32>,
    is_annual: bool,
) -> f64 {
    let sale = package_subscription_amount(list_monthly_price, discount_percent, is_annual);
    if sale <= 0.0 && list_monthly_price > 0.0 {
        list_monthly_price
    } else {
        sale
    }
}
