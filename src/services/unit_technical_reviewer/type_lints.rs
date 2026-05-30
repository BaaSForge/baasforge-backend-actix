//! Syntax-level lint rules (OXC AST). Type-aware `no-unsafe-type-assertion` runs in `lint_checker` via tsgolint.

pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "No explicit `any`",
            "Do not use the `any` type or `as any`. Use `unknown` and narrow, or add proper types.",
        ),
        (
            "Named imports only",
            "`import * as … from \"…\"` is forbidden in all unit code. Use `import { … }` or \
             `import type { … }` instead.",
        ),
        (
            "No unsafe type assertions",
            "Do not use type assertions the checker cannot prove safe; prefer narrowing or typed APIs.",
        ),
    ]
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn run_syntax_lint_checks(_code: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "unit_technical_reviewer")]
use oxc_allocator::Allocator;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_ast::ast::{ImportDeclarationSpecifier, TSType};
#[cfg(feature = "unit_technical_reviewer")]
use oxc_ast_visit::{Visit, walk};
#[cfg(feature = "unit_technical_reviewer")]
use oxc_parser::Parser;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_span::{GetSpan, SourceType};

#[cfg(feature = "unit_technical_reviewer")]
#[derive(Debug, Default)]
struct SyntaxLintFinder<'s> {
    source: &'s str,
    violations: Vec<String>,
}

#[cfg(feature = "unit_technical_reviewer")]
impl<'a, 's> Visit<'a> for SyntaxLintFinder<'s> {
    fn visit_ts_type(&mut self, ty: &TSType<'a>) {
        if matches!(ty, TSType::TSAnyKeyword(_)) {
            let (line, col) = span_line_column(self.source, ty.span().start);
            self.violations.push(format!(
                "Explicit `any` is not allowed (line {line}, col {col}). Use `unknown` and narrow, or add proper types."
            ));
        }
        walk::walk_ts_type(self, ty);
    }

    fn visit_import_declaration(&mut self, decl: &oxc_ast::ast::ImportDeclaration<'a>) {
        let Some(specifiers) = &decl.specifiers else {
            walk::walk_import_declaration(self, decl);
            return;
        };
        let source_module = decl.source.value.as_str();
        for spec in specifiers.iter() {
            if let ImportDeclarationSpecifier::ImportNamespaceSpecifier(ns) = spec {
                let (line, col) = span_line_column(self.source, ns.span().start);
                self.violations.push(format!(
                    "Namespace import `import * as … from \"{source_module}\"` is not allowed (line {line}, col {col}). Use named imports instead."
                ));
            }
        }
        walk::walk_import_declaration(self, decl);
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn span_line_column(source: &str, byte_offset: u32) -> (usize, usize) {
    let byte_offset = byte_offset as usize;
    let byte_offset = byte_offset.min(source.len());
    let prefix = &source[..byte_offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let column = byte_offset.saturating_sub(line_start) + 1;
    (line, column)
}

#[cfg(feature = "unit_technical_reviewer")]
pub fn run_syntax_lint_checks(code: &str) -> Result<(), String> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, code, source_type).parse();

    if !ret.errors.is_empty() {
        let msg = ret
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("Syntax error(s): {msg}"));
    }

    let mut finder = SyntaxLintFinder {
        source: code,
        violations: Vec::new(),
    };
    finder.visit_program(&ret.program);

    if finder.violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Lint violations:\n{}",
            finder.violations.join("\n")
        ))
    }
}

#[cfg(all(test, feature = "unit_technical_reviewer"))]
mod tests {
    use super::run_syntax_lint_checks;

    #[test]
    fn rejects_explicit_any_annotation() {
        assert!(run_syntax_lint_checks("const x: any = 1;").is_err());
    }

    #[test]
    fn rejects_as_any() {
        assert!(run_syntax_lint_checks("const x = (foo as any).bar;").is_err());
    }

    #[test]
    fn allows_as_unknown() {
        assert!(run_syntax_lint_checks("const x = JSON.parse(s) as unknown;").is_ok());
    }

    #[test]
    fn rejects_namespace_import() {
        assert!(run_syntax_lint_checks("import * as fs from \"node:fs\";").is_err());
    }

    #[test]
    fn allows_named_import() {
        assert!(run_syntax_lint_checks("import { parse } from \"node:querystring\";").is_ok());
    }
}
