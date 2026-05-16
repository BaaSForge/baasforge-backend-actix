use crate::models::projects::ProjectUnitType;
use regex::Regex;
use std::sync::LazyLock;

pub static SLUG_PATTERN: &str = r"^[a-z]+(-[a-z]+)*$";

static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SLUG_PATTERN).expect("static slug regex"));

pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty() && SLUG_RE.is_match(s)
}

/// Trims whitespace and lowercases ASCII letters for LLM-supplied slug segments
/// (`context_slug`, `module_slug`, `unit_name_slug`, `context_slugs[]`, etc.).
pub fn normalize_llm_slug_arg(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// Trims and lowercases a full `unit_id` (or path) from the LLM. Digits, dots, and hyphens are unchanged
/// aside from letter casing in slug segments.
pub fn normalize_full_unit_id_arg(s: &str) -> String {
    s.trim().to_ascii_lowercase()
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
        context_slug, module_slug, unit_type_slug, unit_name_slug, revision
    )
}

/// For LLM system prompts and tool schemas: full unit ids are dot-separated; the **last segment is
/// only the revision number** (decimal digits). Do not prefix the revision with `v`, `rev`, or any
/// other text or braces. Correct: `demo.greeter.api.greeter-api.1`. Wrong: `...v1`, `...rev1`,
/// `...{anything}1`.
pub const LLM_UNIT_ID_REVISION_RULE: &str = "The last segment is the revision as decimal digits only (no `v`, `rev`, or other prefix). Correct example: `demo.greeter.api.greeter-api.1`. Invalid: `demo.greeter.api.greeter-api.v1`, `demo.greeter.api.greeter-api.rev1`, or any form like `...{anything}1`.";

/// Tool parameter description for `unit_id` arguments: copy verbatim from list/tool results.
pub const LLM_UNIT_ID_TOOL_DESCRIPTION: &str = concat!(
    "Exact `id` from list or tool results: context.module.unit_kind.unit_name.revision (dot-separated)—copy the whole string verbatim. ",
    "Slug segments use ASCII lowercase;",
    "The last segment is only the revision number (decimal digits), with no prefix such as `v` or `rev` ",
    "(e.g. `demo.greeter.api.greeter-api.1`; not `...v1` or `...rev1`).",
);

/// `finish_architecture_analysis.relevant_unit_ids` array item / field description for LLM schemas.
pub const LLM_RELEVANT_UNIT_IDS_DESCRIPTION: &str = concat!(
    "Most relevant unit IDs for this analysis: each array element is one full unit id string ",
    "(dot-separated: context.module.kind.name.revision), as returned by tools or list payloads. ",
    "Slug segments are lowercase; prefer copying tool output exactly. ",
    "The last segment is only the revision number (decimal digits), with no `v`, `rev`, or other prefix ",
    "(e.g. `demo.greeter.api.greeter-api.1`; not `...v1` or `...rev1`). ",
    "Use an empty array when there are no related units.",
);

/// `finish_implementation.mutated_unit_ids`: units this run created, modified, or removed (successful mutations only).
pub const LLM_MUTATED_UNIT_IDS_DESCRIPTION: &str = concat!(
    "Full unit ids this implementation run created, modified, or removed: each element is one full unit id string ",
    "(dot-separated: context.module.kind.name.revision)",
    "Include every unit you successfully created, modified, or removed. ",
    "Slug segments are lowercase; prefer copying tool output exactly. ",
    "The last segment is only the revision number (decimal digits), with no `v`, `rev`, or other prefix ",
    "(e.g. `demo.greeter.api.greeter-api.1`; not `...v1` or `...rev1`). ",
    "Use an empty array when the directive required no unit mutations.",
);

/// Per-item schema description for a single full unit id in an array.
pub const LLM_FULL_UNIT_ID_ITEM_DESCRIPTION: &str = concat!(
    "Full unit id (dot-separated: context.module.kind.name.revision). ",
    "Slug segments are ASCII lowercase. ",
    "Revision is decimal digits only, no `v`/`rev` prefix (see `demo.greeter.api.greeter-api.1`).",
);

/// Dot-separated path to a logical unit: `context.module.unit_type.unit_name` (everything in a
/// stored unit id except the final revision segment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitPath {
    pub context_slug: String,
    pub module_slug: String,
    pub unit_type: ProjectUnitType,
    pub unit_name_slug: String,
}

/// Validates a **full** unit id for LLM tool arguments: five segments, last segment ASCII digits only
/// (no `v`/`rev`/other prefix), then [`parse_unit_path`] for slug and unit type.
pub fn validate_full_unit_id_for_tool(unit_id: &str) -> Result<(), String> {
    validate_full_unit_id_revision_segment(unit_id)?;
    parse_unit_path(unit_id).map(|_| ())
}

/// Revision segment only: five dot-separated segments; last segment must be decimal digits with no
/// letter prefix (so `v1` / `rev1` are rejected with an explicit message).
pub fn validate_full_unit_id_revision_segment(unit_id: &str) -> Result<(), String> {
    let parts: Vec<&str> = unit_id.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() != 5 {
        return Err(format!(
            "expected exactly five dot-separated segments (context.module.unit_type.unit_name.revision); got {}. The revision must be a plain number with no prefix (e.g. ...greeter-api.1).",
            parts.len()
        ));
    }
    let rev_seg = parts[4];
    if rev_seg.is_empty() {
        return Err("revision segment is empty".to_string());
    }
    if !rev_seg.chars().all(|c| c.is_ascii_digit()) {
        return Err(
            "the last segment (revision) must be digits only, with no prefix such as `v` or `rev` before the number—copy the id from tool or list results (e.g. ...api.1 not ...api.v1)".to_string(),
        );
    }
    let rev_n: i32 = rev_seg
        .parse()
        .map_err(|_| "revision number is too large or invalid".to_string())?;
    if rev_n < 1 {
        return Err("revision must be >= 1".to_string());
    }
    Ok(())
}

/// Parses a [`UnitPath`] from a string.
///
/// - **Full unit id** — five segments, last segment the revision as **decimal digits only** (no
///   `v`/`rev` prefix; parse as a positive integer). Same path as the first four segments (revision
///   is ignored for lookup).
/// - **Path only** — four segments: `context.module.unit_type.unit_name`.
pub fn parse_unit_path(s: &str) -> Result<UnitPath, String> {
    let parts: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
    let (a, b, c, d) = match parts.len() {
        5 => {
            let rev_seg = parts[4];
            if !rev_seg.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!(
                    "invalid unit path: revision segment `{}` must be decimal digits only with no prefix (e.g. `...unit.1`, not `...unit.v1` or `...unit.rev1`)",
                    rev_seg
                ));
            }
            let rev: i32 = rev_seg.parse().map_err(|_| {
                format!(
                    "invalid unit path: revision `{}` is out of i32 range",
                    rev_seg
                )
            })?;
            if rev < 1 {
                return Err(format!(
                    "invalid unit path: revision must be >= 1, got `{}`",
                    rev
                ));
            }
            (parts[0], parts[1], parts[2], parts[3])
        }
        4 => (parts[0], parts[1], parts[2], parts[3]),
        n => {
            return Err(format!(
                "invalid unit path `{}`: expected 4 segments (context.module.unit_type.unit_name) or 5 with a revision, got {}",
                s, n
            ));
        }
    };
    for (name, val) in [
        ("context_slug", a),
        ("module_slug", b),
        ("unit_name_slug", d),
    ] {
        if !is_valid_slug(val) {
            return Err(format!(
                "invalid unit path: `{}` segment `{}` is not a valid slug (must match `{}`)",
                name, val, SLUG_PATTERN
            ));
        }
    }
    let unit_type = ProjectUnitType::from_slug(c).ok_or_else(|| {
        let valid = ProjectUnitType::all()
            .iter()
            .map(|t| format!("`{}`", t.as_slug()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "invalid unit path: unit_type `{}` is unknown; valid values are {}",
            c, valid
        )
    })?;
    Ok(UnitPath {
        context_slug: a.to_string(),
        module_slug: b.to_string(),
        unit_type,
        unit_name_slug: d.to_string(),
    })
}
