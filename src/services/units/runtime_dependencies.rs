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

fn npm_package_use_case(name: &str) -> &'static str {
    match name {
        "zod" => "request/response schemas, parsing, coercion, safe defaults",
        "validator" => "emails, URLs, strings, normalization, sanitization helpers",
        "qs" => "query-string parse/stringify (including nested objects)",
        "cookie" => "parse `Cookie` headers and serialize `Set-Cookie`",
        "http-errors" => "typed HTTP errors with status codes",
        "statuses" => "status code ↔ message helpers",
        "mime-types" => "MIME lookup and content-type helpers",
        "change-case" => "camelCase, kebab-case, snake_case, title case, etc.",
        "slugify" => "URL-safe slugs from arbitrary strings",
        "pluralize" => "singular/plural word forms",
        "escape-html" => "escape text for HTML output",
        "xss" => "sanitize untrusted HTML/strings for safe output",
        "@mjackson/multipart-parser" => "multipart/form-data bodies and file fields",
        _ => "platform-supported utility",
    }
}

fn build_npm_packages_guide() -> String {
    ALLOWED_NPM_SPECS
        .iter()
        .map(|(name, ver)| {
            format!(
                "- **`{name}@{ver}`** — {}",
                npm_package_use_case(name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Short **CRITICAL** block for the solution analyzer system prompt (planning).
pub const RUNTIME_LIBRARIES_PLANNING_DIRECTIVE: &str = r#"## CRITICAL — Plan with allowlisted runtime libraries (mandatory)

Unit code runs in a **closed** Node.js runtime: only the npm packages and `node:*` modules in **Runtime libraries available in unit code** (below) may be imported. There is no `npm install` and no other libraries.

**Default to these libraries** for every concern they cover. Do **not** plan hand-rolled query parsers, cookie helpers, validators, serializers, slug/case transforms, MIME/status tables, multipart parsers, HTML escape/sanitize logic, or crypto/URL primitives when an allowlisted package or `node:*` export already solves it.

In **`detailed_report`**, for each API/actor behavior that touches validation, query strings, cookies, HTTP errors, strings, multipart, HTML safety, or crypto: name the **exact** allowlisted package or `node:*` module (e.g. `zod` for body schemas, `validator` for email format, `qs` for query parsing, `cookie` for sessions, `http-errors` + `statuses` for errors, `node:crypto` for hashing/tokens). Omitting this forces the implementer to reinvent logic that often fails lint or wastes tokens."#;

/// Short **CRITICAL** block for the programmer (implementation) system prompt.
pub const RUNTIME_LIBRARIES_IMPLEMENTATION_DIRECTIVE: &str = r#"## CRITICAL — Use allowlisted runtime libraries (mandatory)

You may import **only** the npm packages and `node:*` modules listed under **Runtime libraries available in unit code** at the end of this prompt. Lint **rejects** any other npm import.

**Use these libraries by default**—do not reimplement their behavior inline. If the directive or your plan names a library, **import it** in unit `code`. Common mistakes to avoid: manual query-string splitting instead of `qs`; ad-hoc email regex instead of `validator`; raw status integers without `http-errors`/`statuses`; custom case/slug helpers instead of `change-case`/`slugify`; DIY multipart parsing instead of `@mjackson/multipart-parser`; string replaces for HTML safety instead of `escape-html`/`xss`; rolling crypto/URL helpers instead of `node:crypto` / `node:url`. When several packages apply, use the most specific one."#;

/// Markdown section listing allowlisted npm packages and `node:*` imports for unit code, with guidance to prefer them over hand-rolled equivalents.
pub fn allowed_runtime_imports_prompt_section() -> String {
    format!(
        "## Runtime libraries available in unit code\n\n\
         Units run in a constrained Node.js TypeScript runtime. Imports are **allowlist-only** \
         (enforced by the linter after every code change).\n\n\
         **Mandatory default:** reach for the packages and `node:*` modules below before writing \
         custom helpers. Hand-rolled equivalents are a defect unless no allowlisted option fits—and \
         you must say so explicitly in the plan or report.\n\n\
         ### npm packages — what to use (import by package name)\n\n\
         {}\n\n\
         ### `node:*` built-ins\n\n\
         {}\n\n\
         ### Lint constraints (exact allowlist)\n\n\
         - **Only supported `node:*` imports**: {}\n\
         - **Only supported npm packages**: {}\n",
        build_npm_packages_guide(),
        "Prefer `node:crypto` (hashing, HMAC, tokens, `randomUUID`, `timingSafeEqual`), \
         `node:url` (`URL`), `node:buffer` (`Buffer`), `node:util` (`TextEncoder`/`TextDecoder`), \
         `node:querystring` (`parse`), `node:http` types for handlers, `node:stream/consumers` (`text`) \
         for bodies, `node:timers/promises` (`setTimeout`)—not custom crypto, URL, or buffer code.",
        *NODE_IMPORTS_LLM_HINT,
        *NPM_PACKAGES_LLM_HINT,
    )
}
