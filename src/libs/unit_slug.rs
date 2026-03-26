use crate::models::projects::ProjectUnitType;
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

/// Dot-separated path to a logical unit: `context.module.unit_type.unit_name` (everything in a
/// stored unit id except the final revision segment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitPath {
    pub context_slug: String,
    pub module_slug: String,
    pub unit_type: ProjectUnitType,
    pub unit_name_slug: String,
}

/// Parses a [`UnitPath`] from a string.
///
/// - **Full unit id** — five segments, last segment a positive integer revision: same path as the
///   first four segments (revision is ignored for lookup).
/// - **Path only** — four segments: `context.module.unit_type.unit_name`.
pub fn parse_unit_path(s: &str) -> Result<UnitPath, &'static str> {
    let parts: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
    let (a, b, c, d) = match parts.len() {
        5 => {
            let rev: i32 = parts[4]
                .parse()
                .map_err(|_| "invalid unit path: revision must be a positive integer")?;
            if rev < 1 {
                return Err("invalid unit path: revision must be >= 1");
            }
            (parts[0], parts[1], parts[2], parts[3])
        }
        4 => (parts[0], parts[1], parts[2], parts[3]),
        _ => {
            return Err(
                "invalid unit path: expected four segments (context.module.unit_type.unit_name) or five with a revision",
            );
        }
    };
    for p in [a, b, c, d] {
        if !is_valid_slug(p) {
            return Err("invalid unit path: each segment must be a valid slug");
        }
    }
    let unit_type =
        ProjectUnitType::from_slug(c).ok_or("invalid unit path: unknown unit_type")?;
    Ok(UnitPath {
        context_slug: a.to_string(),
        module_slug: b.to_string(),
        unit_type,
        unit_name_slug: d.to_string(),
    })
}
