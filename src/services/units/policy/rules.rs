pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![]
}

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<crate::services::unit_technical_reviewer::rule_checker::Rule> {
    vec![]
}
