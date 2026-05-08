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
             createDecipheriv, subtle, getRandomValues } from \"node:crypto\"`, \
             `import { Buffer } from \"node:buffer\"`, \
             `import { TextEncoder, TextDecoder } from \"node:util\"`, \
             `import { setTimeout as setTimeoutPromise } from \"node:timers/promises\"`.",
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
    vec![no_console, no_process, only_allowed_node_imports]
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
fn only_allowed_node_imports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };

    // Static allowlist: module source → permitted imported names (original, not aliases).
    const ALLOWED: &[(&str, &[&str])] = &[
        ("node:http", &["IncomingMessage", "ServerResponse"]),
        ("node:querystring", &["parse"]),
        ("node:stream/consumers", &["text"]),
        ("node:url", &["URL"]),
        (
            "node:crypto",
            &[
                "randomUUID",
                "randomBytes",
                "randomInt",
                "randomFillSync",
                "createHash",
                "createHmac",
                "getHashes",
                "getCiphers",
                "pbkdf2Sync",
                "scryptSync",
                "timingSafeEqual",
                "createCipheriv",
                "createDecipheriv",
                "subtle",
                "getRandomValues",
            ],
        ),
        ("node:buffer", &["Buffer"]),
        ("node:util", &["TextEncoder", "TextDecoder"]),
        ("node:timers/promises", &["setTimeout"]),
    ];

    let mut errors: Vec<String> = Vec::new();

    for src in ast_utils::get_node_module_import_sources(program) {
        // Reject any node: module not in the allowlist.
        let Some(allowed_names) =
            ALLOWED.iter().find_map(
                |(mod_src, names)| {
                    if *mod_src == src { Some(*names) } else { None }
                },
            )
        else {
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
