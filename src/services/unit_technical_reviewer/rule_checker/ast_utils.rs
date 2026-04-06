// Pure utilities for rule composition.
//
// Two categories:
//   1. Source-text utilities — work on raw `&str` without touching the AST.
//   2. OXC AST utilities    — work on parsed `&Program` and sub-nodes.
//
// All functions are pure: no side effects, no error accumulation.

use oxc_ast::ast::{
    Declaration, ExportAllDeclaration, ExportNamedDeclaration, IdentifierReference,
    ModuleExportName, Program, Statement, TSType,
};
use oxc_ast_visit::{walk, Visit};

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
pub fn get_export_named_decls<'a>(
    program: &'a Program<'a>,
) -> Vec<&'a ExportNamedDeclaration<'a>> {
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
pub(super) fn module_export_name_str<'a>(name: &'a ModuleExportName<'a>) -> &'a str {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.as_str(),
        ModuleExportName::IdentifierReference(id) => id.name.as_str(),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

/// Returns the `from '…'` source string of an `export * from '…'` declaration.
pub(super) fn export_all_source<'a>(e: &'a ExportAllDeclaration<'a>) -> &'a str {
    e.source.value.as_str()
}
