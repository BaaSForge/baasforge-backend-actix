use crate::libs::unit_slug::{
    normalize_full_unit_id_arg, parse_unit_path_only, validate_full_unit_id_for_tool,
};
use crate::models::projects::ProjectUnitResponse;
use std::collections::HashSet;

pub const MAX_UNIT_IDS_OR_PATHS: usize = 100;

pub const ARG_UNIT_IDS_DESCRIPTION: &str = "Required. One or more full unit ids to load (each includes revision as the last dot-separated segment). Returns the **exact revision** named in each id when that revision is still not upgraded to a newer revision. Full unit id (dot-separated: context.module.kind.name.revision). Slug segments are ASCII lowercase. Revision is decimal digits only, no `v`/`rev` prefix (e.g. `demo.greeter.api.greeter-api.1`).";

pub const ARG_UNIT_PATHS_DESCRIPTION: &str = "Required. One or more unit paths: `context.module.unit_type.unit_name` (exactly four dot-separated segments—no revision). Do not pass a full unit id (five segments); use load_specific_production_release_units with unit_ids for a specific revision. Returns the **latest revision** of each logical unit. Slug segments are ASCII lowercase.";

pub fn normalize_dedupe_unit_ids(raw: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in raw {
        let normalized = normalize_full_unit_id_arg(id);
        let t = normalized.trim();
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

pub fn normalize_dedupe_unit_paths(raw: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for path in raw {
        let t = path.trim().to_ascii_lowercase();
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == &t) {
            out.push(t);
        }
    }
    out
}

pub fn parse_unit_paths(raw: &[String]) -> Result<Vec<crate::libs::unit_slug::UnitPath>, String> {
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for path_str in raw {
        let t = path_str.trim();
        if t.is_empty() {
            continue;
        }
        match parse_unit_path_only(t) {
            Ok(p) => paths.push(p),
            Err(msg) => errors.push(format!("`{t}`: {msg}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(paths)
}

pub fn validate_unit_ids(ids: &[String]) -> Result<(), String> {
    let mut errors = Vec::new();
    for id in ids {
        if let Err(msg) = validate_full_unit_id_for_tool(id) {
            errors.push(format!("`{id}`: {msg}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub fn missing_unit_ids_message(requested: &[String], units: &[ProjectUnitResponse]) -> Option<String> {
    let found: HashSet<&str> = units.iter().map(|u| u.id.as_str()).collect();
    let missing: Vec<&str> = requested
        .iter()
        .map(|s| s.as_str())
        .filter(|id| !found.contains(id))
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "No unit found for id(s): {}.",
            missing.join(", ")
        ))
    }
}

pub fn missing_unit_paths_message(
    paths: &[crate::libs::unit_slug::UnitPath],
    units: &[ProjectUnitResponse],
) -> Option<String> {
    let found: HashSet<(
        &str,
        &str,
        crate::models::projects::ProjectUnitType,
        &str,
    )> = units
        .iter()
        .map(|u| {
            (
                u.context_slug.as_str(),
                u.module_slug.as_str(),
                u.unit_type,
                u.unit_name_slug.as_str(),
            )
        })
        .collect();
    let mut missing = Vec::new();
    for p in paths {
        let key = (
            p.context_slug.as_str(),
            p.module_slug.as_str(),
            p.unit_type,
            p.unit_name_slug.as_str(),
        );
        if !found.contains(&key) {
            missing.push(format!(
                "{}.{}.{}.{}",
                p.context_slug,
                p.module_slug,
                p.unit_type.as_slug(),
                p.unit_name_slug
            ));
        }
    }
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "No unit found for path(s): {}.",
            missing.join(", ")
        ))
    }
}
