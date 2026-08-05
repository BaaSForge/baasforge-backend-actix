use super::config::ANNUAL_BILLING_DISCOUNT;

/// Amount charged via Stripe for a package purchase.
///
/// Package / annual discounts lower the **sale price**. Balance credit uses the
/// list price instead — see [`package_balance_credit_amount`]. The difference is
/// promotional and is not withdrawable.
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
/// Credits the **list price** (no package or annual discount). Example: list $15
/// with discounts that make the Stripe charge $10 → balance +$15. Free plans
/// (100% off) still credit list price while charging $0.
pub fn package_balance_credit_amount(
    list_monthly_price: f64,
    _discount_percent: Option<i32>,
    is_annual: bool,
) -> f64 {
    if is_annual {
        list_monthly_price * 12.0
    } else {
        list_monthly_price
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paid_package_credits_list_not_sale() {
        let sale = package_subscription_amount(15.0, Some(40), false);
        assert!((sale - 9.0).abs() < 1e-9);
        assert_eq!(package_balance_credit_amount(15.0, Some(40), false), 15.0);
    }

    #[test]
    fn annual_credits_full_list_year() {
        assert_eq!(
            package_subscription_amount(10.0, Some(0), true),
            10.0 * 12.0 * 0.8
        );
        assert_eq!(package_balance_credit_amount(10.0, Some(0), true), 120.0);
    }

    #[test]
    fn free_plan_credits_list_while_charging_zero() {
        assert_eq!(package_subscription_amount(5.0, Some(100), false), 0.0);
        assert_eq!(package_balance_credit_amount(5.0, Some(100), false), 5.0);
    }
}
