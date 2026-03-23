//! Helpers for semver strings stored on project units (not a full semver parser).

/// Increments the major component of a semver string (e.g. `1.2.3` → `2.0.0`). Leading `v` is ignored.
pub fn bump_major_version(version: &str) -> Result<String, String> {
    let trimmed = version.trim();
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed).trim_start();
    let major_str = trimmed
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "empty version".to_string())?;
    let major: u64 = major_str
        .parse()
        .map_err(|_| format!("invalid major in version: {}", version))?;
    Ok(format!("{}.0.0", major + 1))
}
