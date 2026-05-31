//! Allowlisted npm packages and `node:*` imports for unit TypeScript code.
//!
//! Single source of truth for the technical reviewer, LLM agent prompts, and lint messages.
//! Keep versions aligned with `src/tsgo/package.json` (and the runtime project's package.json).

use std::sync::LazyLock;

/// Supported `node:*` import sources and their permitted named exports (original names, not aliases).
pub const ALLOWED_NODE_IMPORTS: &[(&str, &[&str])] = &[
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
            "webcrypto",
        ],
    ),
    ("node:buffer", &["Buffer"]),
    ("node:util", &["TextEncoder", "TextDecoder"]),
    ("node:timers/promises", &["setTimeout"]),
];

macro_rules! allowed_npm_specs {
    ($(($name:literal, $version:literal)),* $(,)?) => {
        pub const ALLOWED_NPM_SPECS: &[(&str, &str)] = &[$(($name, $version)),*];
        pub const ALLOWED_NPM_PACKAGES: &[&str] = &[$($name),*];
    };
}

allowed_npm_specs!(
    ("@mjackson/multipart-parser", "^0.10.1"),
    ("change-case", "^5.4.4"),
    ("cookie", "^1.1.1"),
    ("escape-html", "^1.0.3"),
    ("http-errors", "^2.0.1"),
    ("mime-types", "^3.0.2"),
    ("pluralize", "^8.0.0"),
    ("qs", "^6.14.0"),
    ("slugify", "^1.6.9"),
    ("statuses", "^2.0.2"),
    ("validator", "^13.15.35"),
    ("xss", "^1.0.15"),
    ("zod", "^4.4.3"),
);

pub fn is_allowed_node_import_source(source: &str) -> bool {
    ALLOWED_NODE_IMPORTS
        .iter()
        .any(|(mod_src, _)| *mod_src == source)
}

pub fn allowed_names_for_node_import(source: &str) -> Option<&'static [&'static str]> {
    ALLOWED_NODE_IMPORTS
        .iter()
        .find_map(|(mod_src, names)| {
            if *mod_src == source {
                Some(*names)
            } else {
                None
            }
        })
}

pub fn is_allowed_npm_package_name(name: &str) -> bool {
    ALLOWED_NPM_PACKAGES.iter().any(|pkg| *pkg == name)
}

pub fn allowed_node_module_sources_joined() -> String {
    ALLOWED_NODE_IMPORTS
        .iter()
        .map(|(source, _)| *source)
        .collect::<Vec<_>>()
        .join(", ")
}

fn build_node_imports_llm_hint() -> String {
    let examples: Vec<String> = ALLOWED_NODE_IMPORTS
        .iter()
        .map(|(source, names)| {
            if *source == "node:http" {
                format!(
                    "`import type {{ {} }} from \"{source}\"`",
                    names.join(", ")
                )
            } else {
                format!("`import {{ {} }} from \"{source}\"`", names.join(", "))
            }
        })
        .collect();
    format!(
        "Only the following named exports from Node built-in modules are supported — \
         no other `node:*` modules or names may be imported. \
         Allowed imports: {}.",
        examples.join(", ")
    )
}

fn build_npm_packages_llm_hint() -> String {
    let list = ALLOWED_NPM_SPECS
        .iter()
        .map(|(name, ver)| format!("`{name}@{ver}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Only {list} are available. No other npm packages.")
}

/// LLM hint body for the generic rule “Only supported `node:*` imports”.
pub static NODE_IMPORTS_LLM_HINT: LazyLock<&'static str> = LazyLock::new(|| {
    Box::leak(build_node_imports_llm_hint().into_boxed_str())
});

/// LLM hint body for the generic rule “Only supported npm packages”.
pub static NPM_PACKAGES_LLM_HINT: LazyLock<&'static str> = LazyLock::new(|| {
    Box::leak(build_npm_packages_llm_hint().into_boxed_str())
});

/// Markdown section listing allowlisted npm packages and `node:*` imports for unit code, with guidance to prefer them over hand-rolled equivalents.
pub fn allowed_runtime_imports_prompt_section() -> String {
    format!(
        "## Runtime libraries available in unit code\n\n\
         Units run in a constrained Node.js TypeScript runtime. The programmer may import **only** \
         the allowlisted npm packages and `node:*` modules below (enforced by the linter).\n\n\
         When you plan behavior, **prefer** these libraries over describing hand-rolled equivalents—e.g. \
         `qs` for query strings, `cookie` for cookies, `http-errors` for typed HTTP errors, \
         `statuses` / `mime-types` for HTTP helpers, `change-case` / `slugify` / `pluralize` for \
         string shaping, `escape-html` / `xss` for safe HTML output, \
         `@mjackson/multipart-parser` for multipart bodies, `node:crypto` for hashing and tokens, \
         `node:url` for URLs. In **`detailed_report`**, name which library fits each need so the \
         implementer does not reinvent them.\n\n\
         - **Only supported `node:*` imports**: {}\n\
         - **Only supported npm packages**: {}\n",
        *NODE_IMPORTS_LLM_HINT,
        *NPM_PACKAGES_LLM_HINT,
    )
}
