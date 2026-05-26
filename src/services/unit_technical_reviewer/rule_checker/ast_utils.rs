// Pure utilities for rule composition.
//
// Two categories:
//   1. Source-text utilities — work on raw `&str` without touching the AST.
//   2. OXC AST utilities    — work on parsed `&Program` and sub-nodes.
//
// All functions are pure: no side effects, no error accumulation.

use oxc_ast::ast::{
    CallExpression, Declaration, ExportAllDeclaration, ExportDefaultDeclarationKind,
    ExportNamedDeclaration, Expression, Function, IdentifierReference, ImportDeclaration,
    ImportDeclarationSpecifier, ImportOrExportKind, ModuleExportName, Program, Statement,
    TSType, TSTypeName, UnaryExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::operator::UnaryOperator;

// ── Source-text utilities ─────────────────────────────────────────────────────────────────────

/// Returns `true` if `word` appears as a whole identifier in `source`.
///
/// A "whole identifier" occurrence means the character immediately before and after the match
/// (when they exist) are NOT identifier characters (`[A-Za-z0-9_$]`).
///
/// This is intentionally conservative: it fires for `word` inside string literals and comments
/// as well as in active code.  For linting purposes that is the right default — if a word must
/// not appear at all, rejecting even its occurrence in a comment is safer.
///
/// ```ignore
/// assert!(contains_word("console.log(x)", "console"));
/// assert!(!contains_word("consoleHandler()", "console"));
/// assert!(!contains_word("processData()", "process"));
/// assert!(contains_word("process.env.PORT", "process"));
/// ```
pub fn contains_word(source: &str, word: &str) -> bool {
    if word.is_empty() || source.len() < word.len() {
        return false;
    }
    let src = source.as_bytes();
    let w = word.as_bytes();
    let wlen = w.len();

    for i in 0..=(src.len() - wlen) {
        if &src[i..i + wlen] == w {
            let before_ok = i == 0 || !is_ident_byte(src[i - 1]);
            let after_ok = i + wlen >= src.len() || !is_ident_byte(src[i + wlen]);
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

// ── OXC AST utilities ─────────────────────────────────────────────────────────────────────────

/// Returns `true` if an `IdentifierReference` with the given `name` appears anywhere in the AST.
///
/// Unlike [`contains_word`], this function only fires on actual identifier references in
/// executable code — it is immune to false positives from comments, string literals, and
/// template literal text.  Use this whenever you need to detect usage of a global (e.g.
/// `console`, `process`) rather than just its textual presence in the source.
pub fn has_identifier_reference(program: &Program, name: &str) -> bool {
    struct Finder<'s> {
        name: &'s str,
        found: bool,
    }

    impl<'a, 's> Visit<'a> for Finder<'s> {
        fn visit_identifier_reference(&mut self, ident: &IdentifierReference<'a>) {
            if ident.name.as_str() == self.name {
                self.found = true;
            }
            walk::walk_identifier_reference(self, ident);
        }
    }

    let mut finder = Finder { name, found: false };
    finder.visit_program(program);
    finder.found
}

/// Returns the names of all top-level exported type aliases, in source order.
pub fn get_exported_type_names<'a>(program: &'a Program) -> Vec<&'a str> {
    program
        .body
        .iter()
        .filter_map(|stmt| {
            if let Statement::ExportNamedDeclaration(export) = stmt {
                if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                    return Some(alias.id.name.as_str());
                }
            }
            None
        })
        .collect()
}

/// Returns `true` if a type alias named `name` is exported at the top level.
pub fn has_exported_type(program: &Program, name: &str) -> bool {
    get_exported_type_names(program).contains(&name)
}

/// Returns a reference to the `TSType` annotation of the exported type alias named `name`.
///
/// ```ignore
/// if let Some(ty) = get_exported_type(program, "ResponseBody") {
///     if is_never(ty) { /* … */ }
/// }
/// ```
pub fn get_exported_type<'a>(program: &'a Program<'a>, name: &str) -> Option<&'a TSType<'a>> {
    program.body.iter().find_map(|stmt| {
        if let Statement::ExportNamedDeclaration(export) = stmt {
            if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                if alias.id.name.as_str() == name {
                    return Some(&alias.type_annotation);
                }
            }
        }
        None
    })
}

/// Returns all top-level `ExportNamedDeclaration` nodes (type and value alike).
pub fn get_export_named_decls<'a>(program: &'a Program<'a>) -> Vec<&'a ExportNamedDeclaration<'a>> {
    program
        .body
        .iter()
        .filter_map(|stmt| {
            if let Statement::ExportNamedDeclaration(e) = stmt {
                Some(e.as_ref())
            } else {
                None
            }
        })
        .collect()
}

/// Returns a human-readable description for every export that is NOT a type alias declaration.
///
/// Useful for "no value exports" rules:
/// ```ignore
/// let bad = get_non_type_alias_exports(program);
/// if !bad.is_empty() { /* fail */ }
/// ```
pub fn get_non_type_alias_exports(program: &Program) -> Vec<String> {
    let mut result = Vec::new();
    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    if !matches!(decl, Declaration::TSTypeAliasDeclaration(_)) {
                        result.push(
                            "non-type named export (const / function / class / …)".to_string(),
                        );
                    }
                }
                for spec in &export.specifiers {
                    result.push(format!(
                        "re-export `{}`",
                        module_export_name_str(&spec.exported)
                    ));
                }
            }
            Statement::ExportDefaultDeclaration(_) => {
                result.push("default export".to_string());
            }
            Statement::ExportAllDeclaration(e) => {
                result.push(format!("export * from '{}'", export_all_source(e)));
            }
            _ => {}
        }
    }
    result
}

// ── Type predicates ───────────────────────────────────────────────────────────────────────────

/// Returns `true` if the type is the `never` keyword.
pub fn is_never(ty: &TSType) -> bool {
    matches!(ty, TSType::TSNeverKeyword(_))
}

/// Returns `true` if the type is an inline object literal (`{ field: Type; … }`).
pub fn is_object_type(ty: &TSType) -> bool {
    matches!(ty, TSType::TSTypeLiteral(_))
}

// ── Internal helpers (accessible within the `rule_checker` module family) ─────────────────────

/// Extracts the string name from a `ModuleExportName` (covers ident, reference, and string forms).
pub fn module_export_name_str<'a>(name: &'a ModuleExportName<'a>) -> &'a str {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.as_str(),
        ModuleExportName::IdentifierReference(id) => id.name.as_str(),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

/// Returns the `from '…'` source string of an `export * from '…'` declaration.
pub fn export_all_source<'a>(e: &'a ExportAllDeclaration<'a>) -> &'a str {
    e.source.value.as_str()
}

// ── Import utilities ──────────────────────────────────────────────────────────────────────────

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

/// Bare npm package names allowed in unit imports (no version suffix).
pub const ALLOWED_NPM_PACKAGES: &[&str] = &["qs", "cookie", "statuses"];

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

/// Root package name for a bare npm import (`qs`, `qs/lib`, `@scope/pkg/…`), or `None` for
/// relative paths, `node:*`, and `@units/…`.
pub fn npm_package_name_from_import_source(source: &str) -> Option<&str> {
    if source.is_empty()
        || source.starts_with('.')
        || source.starts_with('/')
        || source.starts_with("node:")
        || source.starts_with("@units/")
    {
        return None;
    }
    if let Some(stripped) = source.strip_prefix('@') {
        let rest = stripped;
        let scope_end = rest.find('/')?;
        let after_scope = &rest[scope_end + 1..];
        let name_end = after_scope.find('/').unwrap_or(after_scope.len());
        let end = 1 + scope_end + 1 + name_end;
        return Some(&source[..end]);
    }
    let name_end = source.find('/').unwrap_or(source.len());
    Some(&source[..name_end])
}

pub fn is_allowed_npm_import_source(source: &str) -> bool {
    npm_package_name_from_import_source(source)
        .is_some_and(is_allowed_npm_package_name)
}

/// Returns all distinct bare npm import source strings in the program, in first-occurrence order.
pub fn get_npm_package_import_sources<'a>(program: &'a Program<'a>) -> Vec<&'a str> {
    let mut result: Vec<&str> = Vec::new();
    for decl in get_import_declarations(program) {
        let src = decl.source.value.as_str();
        if npm_package_name_from_import_source(src).is_some() && !result.contains(&src) {
            result.push(src);
        }
    }
    result
}

/// `unit_type` segment of `@units/<context>.<module>.<unit_type>.<unit_name>`, if well-formed.
pub fn unit_type_slug_from_units_import(source: &str) -> Option<&str> {
    let path = source.strip_prefix("@units/")?;
    let segments: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if segments.len() == 4 {
        Some(segments[2])
    } else {
        None
    }
}

pub fn is_units_import_from_common(source: &str) -> bool {
    unit_type_slug_from_units_import(source) == Some("common")
}

/// `true` for bare (`import "…"`), namespace (`import * as …`), or default (`import x from "…"`)
/// forms — every `@units/` import must use explicit named specifiers (`import { a }` / `import type { T }`).
pub fn units_import_without_named_specifiers(decl: &ImportDeclaration) -> bool {
    let Some(specifiers) = &decl.specifiers else {
        return true;
    };
    if specifiers.is_empty() {
        return true;
    }
    !specifiers.iter().any(|spec| {
        matches!(spec, ImportDeclarationSpecifier::ImportSpecifier(_))
    })
}

/// `true` when the import binds at least one runtime (non-type-only) symbol.
pub fn units_import_has_runtime_binding(decl: &ImportDeclaration) -> bool {
    if decl.import_kind == ImportOrExportKind::Type {
        return false;
    }
    let Some(specifiers) = &decl.specifiers else {
        return true;
    };
    for spec in specifiers.iter() {
        match spec {
            ImportDeclarationSpecifier::ImportDefaultSpecifier(_)
            | ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => return true,
            ImportDeclarationSpecifier::ImportSpecifier(s) => {
                if s.import_kind == ImportOrExportKind::Value {
                    return true;
                }
            }
        }
    }
    false
}

/// Keep in compiled JS: runtime imports from `@units/…common…`, allowlisted `node:*`, and
/// allowlisted npm packages.
pub fn should_preserve_compiled_import(decl: &ImportDeclaration) -> bool {
    if !units_import_has_runtime_binding(decl) {
        return false;
    }
    let source = decl.source.value.as_str();
    if source.starts_with("@units/") {
        return is_units_import_from_common(source);
    }
    is_allowed_node_import_source(source) || is_allowed_npm_import_source(source)
}

/// Returns all `ImportDeclaration` nodes in the program, in source order.
pub fn get_import_declarations<'a>(program: &'a Program<'a>) -> Vec<&'a ImportDeclaration<'a>> {
    program
        .body
        .iter()
        .filter_map(|stmt| {
            if let Statement::ImportDeclaration(decl) = stmt {
                Some(decl.as_ref())
            } else {
                None
            }
        })
        .collect()
}

/// Returns all `ImportDeclaration` nodes whose source string equals `source`.
pub fn get_imports_from_source<'a>(
    program: &'a Program<'a>,
    source: &str,
) -> Vec<&'a ImportDeclaration<'a>> {
    get_import_declarations(program)
        .into_iter()
        .filter(|d| d.source.value.as_str() == source)
        .collect()
}

/// Returns the original (imported) names of all named specifiers from `source`.
///
/// For `import { parse as parseQuerystring } from "node:querystring"` this returns `["parse"]`
/// — the *imported* name, not the local alias.  Default and namespace specifiers are excluded.
pub fn get_named_imports_from_source<'a>(program: &'a Program<'a>, source: &str) -> Vec<&'a str> {
    get_imports_from_source(program, source)
        .into_iter()
        .flat_map(|decl| named_specifier_imported_names(decl))
        .collect()
}

/// Returns `true` if there is at least one import declaration (of any kind) from `source`.
pub fn has_import_from(program: &Program, source: &str) -> bool {
    get_import_declarations(program)
        .iter()
        .any(|d| d.source.value.as_str() == source)
}

/// Returns all distinct `node:*` import source strings in the program, in first-occurrence order.
pub fn get_node_module_import_sources<'a>(program: &'a Program<'a>) -> Vec<&'a str> {
    let mut result: Vec<&str> = Vec::new();
    for decl in get_import_declarations(program) {
        let src = decl.source.value.as_str();
        if src.starts_with("node:") && !result.contains(&src) {
            result.push(src);
        }
    }
    result
}

/// Returns `true` if the program contains a namespace import (`import * as x from source`).
pub fn has_namespace_import_from(program: &Program, source: &str) -> bool {
    get_imports_from_source(program, source).iter().any(|decl| {
        decl.specifiers.iter().flat_map(|v| v.iter()).any(|spec| {
            matches!(
                spec,
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(_)
            )
        })
    })
}

/// Returns `true` if the program contains a default import from `source`
/// (i.e. `import name from "source"`).
pub fn has_default_import_from(program: &Program, source: &str) -> bool {
    get_imports_from_source(program, source).iter().any(|decl| {
        decl.specifiers
            .iter()
            .flat_map(|v| v.iter())
            .any(|spec| matches!(spec, ImportDeclarationSpecifier::ImportDefaultSpecifier(_)))
    })
}

// ── Type annotation utilities ─────────────────────────────────────────────────────────────────

/// If `ty` is a plain `TSTypeReference` to a bare identifier (e.g. `MyType`), returns that
/// identifier name.  Returns `None` for qualified names (`A.B`), generics, primitives, etc.
///
/// ```ignore
/// if let Some(name) = ts_type_reference_name(ty) {
///     if name == "IncomingMessage" { /* … */ }
/// }
/// ```
pub fn ts_type_reference_name<'a>(ty: &'a TSType<'a>) -> Option<&'a str> {
    if let TSType::TSTypeReference(tr) = ty {
        if let TSTypeName::IdentifierReference(id) = &tr.type_name {
            return Some(id.name.as_str());
        }
    }
    None
}

// ── Function export utilities ─────────────────────────────────────────────────────────────────

/// Returns `true` if the program has any `export default` statement.
pub fn has_export_default(program: &Program) -> bool {
    program
        .body
        .iter()
        .any(|stmt| matches!(stmt, Statement::ExportDefaultDeclaration(_)))
}

/// If `export default` is a direct call expression (`export default fn(...)`), returns the
/// callee identifier name.  Returns `None` if there's no default export or it's not a plain call.
pub fn get_export_default_call_name<'a>(program: &'a Program<'a>) -> Option<&'a str> {
    let call = get_export_default_call_expression(program)?;
    let callee = peel_parentheses(call_callee_expression(&call.callee));
    if let Expression::Identifier(id) = callee {
        return Some(id.name.as_str());
    }
    None
}

/// If `export default` is a call expression, returns it. Otherwise `None`.
pub fn get_export_default_call_expression<'a>(
    program: &'a Program<'a>,
) -> Option<&'a CallExpression<'a>> {
    for stmt in &program.body {
        if let Statement::ExportDefaultDeclaration(export) = stmt {
            if let ExportDefaultDeclarationKind::CallExpression(call) = &export.declaration {
                return Some(call.as_ref());
            }
        }
    }
    None
}

fn call_callee_expression<'a>(callee: &'a Expression<'a>) -> &'a Expression<'a> {
    let mut cur = callee;
    loop {
        cur = peel_parentheses(cur);
        match cur {
            Expression::TSInstantiationExpression(inst) => {
                cur = &inst.expression;
            }
            _ => return cur,
        }
    }
}

/// Returns the names of all top-level exported function declarations, in source order.
///
/// Only covers `export function name(…)` style — not arrow functions assigned to `const`.
pub fn get_exported_function_names<'a>(program: &'a Program<'a>) -> Vec<&'a str> {
    program
        .body
        .iter()
        .filter_map(|stmt| {
            if let Statement::ExportNamedDeclaration(export) = stmt {
                if let Some(Declaration::FunctionDeclaration(func)) = &export.declaration {
                    return func.id.as_ref().map(|id| id.name.as_str());
                }
            }
            None
        })
        .collect()
}

/// Returns `true` if a function named `name` is exported at the top level.
pub fn has_exported_function(program: &Program, name: &str) -> bool {
    get_exported_function_names(program).contains(&name)
}

/// Returns the number of parameters of the exported function named `fn_name`.
/// Returns `None` if no such exported function is found.
pub fn get_exported_function_param_count(program: &Program, fn_name: &str) -> Option<usize> {
    find_exported_function(program, fn_name).map(|f| f.params.items.len())
}

/// Returns the type-reference name of the `param_index`-th (0-indexed) parameter of the exported
/// function named `fn_name`.
///
/// Returns `None` when:
/// - the function is not found,
/// - the parameter index is out of bounds,
/// - the parameter has no type annotation, or
/// - the type annotation is not a plain `TSTypeReference` to a bare identifier.
///
/// ```ignore
/// assert_eq!(
///     get_exported_function_param_type_name(program, "handleRequest", 0),
///     Some("IncomingMessage"),
/// );
/// ```
pub fn get_exported_function_param_type_name<'a>(
    program: &'a Program<'a>,
    fn_name: &str,
    param_index: usize,
) -> Option<&'a str> {
    let func = find_exported_function(program, fn_name)?;
    let param = func.params.items.get(param_index)?;
    let type_ann = param.type_annotation.as_ref()?;
    ts_type_reference_name(&type_ann.type_annotation)
}

/// Returns `true` if `ty` is exactly `Promise<void>` (a reference named `Promise` with one `void` argument).
pub fn is_promise_void_type(ty: &TSType) -> bool {
    let TSType::TSTypeReference(tr) = ty else {
        return false;
    };
    let TSTypeName::IdentifierReference(id) = &tr.type_name else {
        return false;
    };
    if id.name.as_str() != "Promise" {
        return false;
    }
    let Some(args) = tr.type_arguments.as_deref() else {
        return false;
    };
    if args.params.len() != 1 {
        return false;
    }
    matches!(&args.params[0], TSType::TSVoidKeyword(_))
}

/// Returns whether the exported function named `fn_name` is declared `async`.
pub fn get_exported_function_is_async(program: &Program, fn_name: &str) -> Option<bool> {
    find_exported_function(program, fn_name).map(|f| f.r#async)
}

/// Returns the return type annotation of the exported function named `fn_name`, if present.
pub fn get_exported_function_return_type<'a>(
    program: &'a Program<'a>,
    fn_name: &str,
) -> Option<&'a TSType<'a>> {
    find_exported_function(program, fn_name)?
        .return_type
        .as_deref()
        .map(|ann| &ann.type_annotation)
}

/// Returns `true` if the body of the exported function `fn_name` contains `void (<async fn>() => …)()`
/// (or `void (async function () { … })()`): discarding an immediately-invoked async function so the
/// outer handler returns before the async work finishes.
pub fn exported_function_body_contains_void_discarded_async_invoke(
    program: &Program,
    fn_name: &str,
) -> bool {
    let Some(func) = find_exported_function(program, fn_name) else {
        return false;
    };
    let Some(body) = func.body.as_deref() else {
        return false;
    };
    let mut finder = VoidAsyncInvokeFinder { found: false };
    finder.visit_function_body(body);
    finder.found
}

struct VoidAsyncInvokeFinder {
    found: bool,
}

impl<'a> Visit<'a> for VoidAsyncInvokeFinder {
    fn visit_unary_expression(&mut self, expr: &UnaryExpression<'a>) {
        if !self.found && unary_void_discarded_async_invoke(expr) {
            self.found = true;
            return;
        }
        walk::walk_unary_expression(self, expr);
    }
}

fn unary_void_discarded_async_invoke(u: &UnaryExpression<'_>) -> bool {
    if u.operator != UnaryOperator::Void {
        return false;
    }
    let inner = peel_parentheses(&u.argument);
    let Expression::CallExpression(call) = inner else {
        return false;
    };
    let callee = peel_parentheses(&call.callee);
    match callee {
        Expression::ArrowFunctionExpression(a) => a.r#async,
        Expression::FunctionExpression(f) => f.r#async,
        _ => false,
    }
}

fn peel_parentheses<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
    let mut cur = expr;
    while let Expression::ParenthesizedExpression(p) = cur {
        cur = &p.expression;
    }
    cur
}

// ── Private helpers ───────────────────────────────────────────────────────────────────────────

/// Returns a reference to the `Function` node for the exported function named `name`, or `None`.
fn find_exported_function<'a>(program: &'a Program<'a>, name: &str) -> Option<&'a Function<'a>> {
    program.body.iter().find_map(|stmt| {
        if let Statement::ExportNamedDeclaration(export) = stmt {
            if let Some(Declaration::FunctionDeclaration(func)) = &export.declaration {
                if func
                    .id
                    .as_ref()
                    .map_or(false, |id| id.name.as_str() == name)
                {
                    return Some(func.as_ref());
                }
            }
        }
        None
    })
}

/// Returns the imported names of all `ImportSpecifier` entries in a single `ImportDeclaration`.
/// Default and namespace specifiers are excluded.
fn named_specifier_imported_names<'a>(decl: &'a ImportDeclaration<'a>) -> Vec<&'a str> {
    decl.specifiers
        .iter()
        .flat_map(|v| v.iter())
        .filter_map(|spec| {
            if let ImportDeclarationSpecifier::ImportSpecifier(s) = spec {
                Some(module_export_name_str(&s.imported))
            } else {
                None
            }
        })
        .collect()
}
