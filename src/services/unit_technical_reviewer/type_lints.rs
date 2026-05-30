//! Syntax-level `no-explicit-any` (OXC AST). Type-aware `no-unsafe-type-assertion` runs in `lint_checker` via tsgolint.

#[cfg(feature = "unit_technical_reviewer")]
use oxc_allocator::Allocator;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_ast::ast::TSType;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_ast_visit::{Visit, walk};
#[cfg(feature = "unit_technical_reviewer")]
use oxc_parser::Parser;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_span::{GetSpan, SourceType};

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn run_no_explicit_any(_code: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "unit_technical_reviewer")]
#[derive(Debug, Default)]
struct AnyFinder<'s> {
    source: &'s str,
    violations: Vec<String>,
}

#[cfg(feature = "unit_technical_reviewer")]
impl<'a, 's> Visit<'a> for AnyFinder<'s> {
    fn visit_ts_type(&mut self, ty: &TSType<'a>) {
        if matches!(ty, TSType::TSAnyKeyword(_)) {
            let (line, col) = span_line_column(self.source, ty.span().start);
            self.violations.push(format!(
                "Explicit `any` is not allowed (line {line}, col {col}). Use `unknown` and narrow, or add proper types."
            ));
        }
        walk::walk_ts_type(self, ty);
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
pub fn run_no_explicit_any(code: &str) -> Result<(), String> {
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

    let mut finder = AnyFinder {
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
    use super::run_no_explicit_any;

    #[test]
    fn rejects_explicit_any_annotation() {
        assert!(run_no_explicit_any("const x: any = 1;").is_err());
    }

    #[test]
    fn rejects_as_any() {
        assert!(run_no_explicit_any("const x = (foo as any).bar;").is_err());
    }

    #[test]
    fn allows_as_unknown() {
        assert!(run_no_explicit_any("const x = JSON.parse(s) as unknown;").is_ok());
    }
}
