// Generic rules — applied to every unit type and every operation.
//
// Rules here express invariants that hold across the entire codebase regardless of unit type
// or operation.  Rules that are specific to a single unit type belong in that type's rule file
// (e.g. `units/api_rules.rs`).  Rules that only make sense for one operation belong in the matching
// operation rule file (`create_rules.rs`, `modify_rules.rs`, `remove_rules.rs`).
//
// All rules here use `ast_utils::has_identifier_reference` rather than raw-string searches so
// that comments, string literals, and template literal text cannot produce false positives.
// AST-based rules use the parsed `input.program` (new code) or `input.old_program` (old code).

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for generic rules (all unit types, all operations).
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "No `console` usage",
            "Never use `console` in any unit (e.g. `console.log`, `console.error`, `console.warn`).",
        ),
        (
            "No `process` access",
            "Never access `process` directly in any unit \
             (e.g. `process.env`, `process.exit`, `process.argv`).",
        ),
        (
            "Cross-unit `@units/` imports",
            "Path shape: `@units/<context>.<module>.<unit_type>.<unit_name>`. \
             `import type { … }` from any unit type is allowed. \
             Runtime `import { … }` (without `type`) is allowed only when the path's `unit_type` segment is `common`. \
             Every `@units/…` import must name bindings: `import { … }` or `import type { … }`. \
             Forbidden: bare `import \"@units/…\"`, `import * as … from \"@units/…\"`, and `import x from \"@units/…\"`.",
        ),
        (
            "Only supported `node:*` imports",
            "Only the following named exports from Node built-in modules are supported — \
             no other `node:*` modules or names may be imported. \
             Namespace (`import * as x`) and default imports from `node:*` are forbidden. \
             Allowed imports: \
             `import type { IncomingMessage, ServerResponse } from \"node:http\"`, \
             `import { parse as parseQuerystring } from \"node:querystring\"`, \
             `import { text } from \"node:stream/consumers\"`, \
             `import { URL } from \"node:url\"`, \
             `import { randomUUID, randomBytes, randomInt, randomFillSync, createHash, createHmac, \
             getHashes, getCiphers, pbkdf2Sync, scryptSync, timingSafeEqual, createCipheriv, \
             createDecipheriv, subtle, getRandomValues, webcrypto } from \"node:crypto\"`, \
             `import { Buffer } from \"node:buffer\"`, \
             `import { TextEncoder, TextDecoder } from \"node:util\"`, \
             `import { setTimeout as setTimeoutPromise } from \"node:timers/promises\"`.",
        ),
        (
            "Only supported npm packages",
            "Only `qs@^6.14.0` and `cookie@^1.1.1` are available. \
             No other npm packages.",
        ),
    ]
}

// ── Linting rules (require the OXC parser) ────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use super::Rule;
#[cfg(feature = "unit_technical_reviewer")]
use super::RuleInput;
#[cfg(feature = "unit_technical_reviewer")]
use super::ast_utils;

#[cfg(feature = "unit_technical_reviewer")]
pub(super) fn rules() -> Vec<Rule> {
    vec![
        no_console,
        no_process,
        only_allowed_cross_unit_imports,
        only_allowed_node_imports,
        only_allowed_npm_packages_imports,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

/// Units must not reference the `console` global.
///
/// Server-side units run inside a controlled runtime; structured logging must be used instead of
/// ad-hoc `console.log` / `console.error` calls.  Leaving console statements in production code
/// leaks internal information and bypasses log aggregation.
///
/// Detection is AST-based: only real `console` identifier references in executable code are
/// flagged — occurrences inside comments or string literals are ignored.
#[cfg(feature = "unit_technical_reviewer")]
fn no_console(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if ast_utils::has_identifier_reference(program, "console") {
        vec![
            "Do not use `console` in units (e.g. `console.log`, `console.error`). \
             Use the project's structured logger instead."
                .to_string(),
        ]
    } else {
        vec![]
    }
}

/// Units must not reference the `process` global.
///
/// Directly accessing `process.env`, `process.exit`, `process.argv`, etc. couples a unit to
/// Node.js runtime internals and makes the code harder to test and port.  Environment
/// configuration must flow in through the unit's explicit inputs.
///
/// Detection is AST-based: only real `process` identifier references in executable code are
/// flagged — occurrences inside comments or string literals are ignored.
#[cfg(feature = "unit_technical_reviewer")]
fn no_process(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if ast_utils::has_identifier_reference(program, "process") {
        vec![
            "Do not access `process` directly in units (e.g. `process.env`, `process.exit`). \
             Receive configuration through the unit's explicit inputs."
                .to_string(),
        ]
    } else {
        vec![]
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn only_allowed_cross_unit_imports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };

    let mut errors: Vec<String> = Vec::new();

    for decl in ast_utils::get_import_declarations(program) {
        let source = decl.source.value.as_str();
        if !source.starts_with("@units/") {
            continue;
        }

        if ast_utils::unit_type_slug_from_units_import(source).is_none() {
            errors.push(format!(
                "Import from `{source}` is invalid; expected `@units/<context>.<module>.<unit_type>.<unit_name>` (four segments after `@units/`)"
            ));
            continue;
        }

        if ast_utils::units_import_without_named_specifiers(decl) {
            errors.push(format!(
                "Import from `{source}` must list named bindings, e.g. `import {{ fn }} from \"{source}\"` \
                 or `import type {{ T }} from \"{source}\"`; bare (`import \"…\"`), namespace (`import * as …`), \
                 and default (`import x from …`) forms are forbidden"
            ));
            continue;
        }

        if ast_utils::units_import_has_runtime_binding(decl)
            && !ast_utils::is_units_import_from_common(source)
        {
            errors.push(format!(
                "Runtime import from `{source}` is not allowed; value imports from other units must use a `common` unit (`…module.common.unit-name`). Use `import type` for types from other unit kinds, or the sdk for runtime coupling between non-common units"
            ));
        }
    }

    errors
}

#[cfg(feature = "unit_technical_reviewer")]
fn only_allowed_node_imports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };

    let mut errors: Vec<String> = Vec::new();

    for src in ast_utils::get_node_module_import_sources(program) {
        let Some(allowed_names) = ast_utils::allowed_names_for_node_import(src) else {
            errors.push(format!(
                "Import from `{src}` is not supported in units; \
                 only node:http, node:querystring, node:stream/consumers, node:url, \
                 node:crypto, node:buffer, node:util, and node:timers/promises are allowed"
            ));
            continue;
        };

        // Namespace imports are never allowed.
        if ast_utils::has_namespace_import_from(program, src) {
            errors.push(format!(
                "Namespace import (`import * as x from \"{src}\"`) is not allowed; \
                 use explicit named imports instead"
            ));
        }

        // Default imports are never allowed.
        if ast_utils::has_default_import_from(program, src) {
            errors.push(format!(
                "Default import from `{src}` is not allowed; \
                 use explicit named imports instead"
            ));
        }

        // Each named import must be in the allowlist for this module.
        for name in ast_utils::get_named_imports_from_source(program, src) {
            if !allowed_names.contains(&name) {
                errors.push(format!(
                    "Import `{name}` from `{src}` is not supported; \
                     allowed names: {}",
                    allowed_names.join(", ")
                ));
            }
        }
    }

    errors
}

#[cfg(feature = "unit_technical_reviewer")]
fn only_allowed_npm_packages_imports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };

    let allowed_list = ast_utils::ALLOWED_NPM_PACKAGES.join(", ");
    let mut errors: Vec<String> = Vec::new();
    let mut reported: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for source in ast_utils::get_npm_package_import_sources(program) {
        let Some(pkg) = ast_utils::npm_package_name_from_import_source(source) else {
            continue;
        };
        if ast_utils::is_allowed_npm_package_name(pkg) {
            continue;
        }
        if reported.insert(pkg) {
            errors.push(format!(
                "Import from `{source}` is not supported in units; \
                 only npm packages {allowed_list} are allowed"
            ));
        }
    }

    errors
}
