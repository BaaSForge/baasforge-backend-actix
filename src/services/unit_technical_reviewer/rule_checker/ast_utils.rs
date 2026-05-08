// Pure utilities for rule composition.
//
// Two categories:
//   1. Source-text utilities — work on raw `&str` without touching the AST.
//   2. OXC AST utilities    — work on parsed `&Program` and sub-nodes.
//
// All functions are pure: no side effects, no error accumulation.

use oxc_ast::ast::{
    Declaration, ExportAllDeclaration, ExportDefaultDeclarationKind, ExportNamedDeclaration,
    Expression, Function, IdentifierReference, ImportDeclaration, ImportDeclarationSpecifier,
    ModuleExportName, Program, Statement, TSType, TSTypeName,
};
use oxc_ast_visit::{Visit, walk};

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
    for stmt in &program.body {
        if let Statement::ExportDefaultDeclaration(export) = stmt {
            if let ExportDefaultDeclarationKind::CallExpression(call) = &export.declaration {
                if let Expression::Identifier(id) = &call.callee {
                    return Some(id.name.as_str());
                }
            }
        }
    }
    None
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
