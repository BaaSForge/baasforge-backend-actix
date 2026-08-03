use super::config::ANNUAL_BILLING_DISCOUNT;

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
