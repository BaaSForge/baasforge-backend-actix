use regex::Regex;
use std::sync::LazyLock;

pub static SLUG_PATTERN: &str = r"^[a-z]+(-[a-z]+)*$";

static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SLUG_PATTERN).expect("static slug regex"));

pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty() && SLUG_RE.is_match(s)
}

pub fn compose_unit_id(
    context_slug: &str,
    module_slug: &str,
    unit_type_slug: &str,
    unit_name_slug: &str,
    revision: i32,
) -> String {
    format!(
        "{}.{}.{}.{}.{}",
        context_slug,
        module_slug,
        unit_type_slug,
        unit_name_slug,
        revision
    )
}
