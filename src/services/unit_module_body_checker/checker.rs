use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, Declaration, ExportDefaultDeclarationKind, Expression,
    ObjectPropertyKind, Program, PropertyKey, Statement, VariableDeclaration,
    VariableDeclarationKind, VariableDeclarator,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitProfile {
    Common,
    Api,
    Workflow,
    Actor,
    QueueWorker,
}

pub fn run_check(unit_type: &str, code: &str) -> Vec<String> {
    let Some(profile) = profile_for_unit_type(unit_type) else {
        return vec![];
    };
    if code.is_empty() {
        return vec![];
    }

    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, code, source_type).parse();
    if !ret.errors.is_empty() {
        return vec![];
    }

    let mut errors = Vec::new();
    check_program(&ret.program, profile, &mut errors);
    errors
}

fn profile_for_unit_type(unit_type: &str) -> Option<UnitProfile> {
    match unit_type {
        "common" => Some(UnitProfile::Common),
        "api" => Some(UnitProfile::Api),
        "workflow" => Some(UnitProfile::Workflow),
        "actor" => Some(UnitProfile::Actor),
        "queue_worker" => Some(UnitProfile::QueueWorker),
        _ => None,
    }
}

fn check_program(program: &Program, profile: UnitProfile, errors: &mut Vec<String>) {
    for stmt in &program.body {
        check_top_level_statement(stmt, profile, errors);
    }
}

fn check_top_level_statement(stmt: &Statement, profile: UnitProfile, errors: &mut Vec<String>) {
    match stmt {
        Statement::ImportDeclaration(_) => {}

        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::VariableDeclaration(var) => {
                        check_variable_declaration(var, profile, errors);
                    }
                    Declaration::FunctionDeclaration(_) | Declaration::ClassDeclaration(_) => {}
                    Declaration::TSTypeAliasDeclaration(_)
                    | Declaration::TSInterfaceDeclaration(_)
                    | Declaration::TSModuleDeclaration(_)
                    | Declaration::TSImportEqualsDeclaration(_) => {}
                    _ => push_forbidden_top_level_stmt(profile, errors),
                }
            }
        }

        Statement::ExportDefaultDeclaration(export) => match profile {
            UnitProfile::Common | UnitProfile::Api => {
                errors.push(
                    "Module body must not use `export default`; side effects must not hide behind a default export"
                        .to_string(),
                );
            }
            UnitProfile::Workflow | UnitProfile::Actor | UnitProfile::QueueWorker => {
                check_export_default_factory(&export.declaration, profile, errors);
            }
        },

        Statement::VariableDeclaration(var) => match profile {
            UnitProfile::Common => {
                errors.push(
                    "Common unit module body must not declare top-level `const`/`let`; use `export` only"
                        .to_string(),
                );
            }
            UnitProfile::Api
            | UnitProfile::Workflow
            | UnitProfile::Actor
            | UnitProfile::QueueWorker => {
                check_variable_declaration(var, profile, errors);
            }
        },

        Statement::ExpressionStatement(_) => {
            errors.push(
                "Module body must not contain top-level expression statements (no `foo();`, IIFEs, or bare calls at import time)"
                    .to_string(),
            );
        }

        Statement::ExportAllDeclaration(_) => {
            errors.push(
                "Module body must not use `export * from '…'` at top level".to_string(),
            );
        }

        Statement::BlockStatement(_)
        | Statement::EmptyStatement(_)
        | Statement::DebuggerStatement(_)
        | Statement::DoWhileStatement(_)
        | Statement::ForInStatement(_)
        | Statement::ForOfStatement(_)
        | Statement::ForStatement(_)
        | Statement::IfStatement(_)
        | Statement::LabeledStatement(_)
        | Statement::ReturnStatement(_)
        | Statement::SwitchStatement(_)
        | Statement::ThrowStatement(_)
        | Statement::TryStatement(_)
        | Statement::WhileStatement(_)
        | Statement::WithStatement(_)
        | Statement::BreakStatement(_)
        | Statement::ContinueStatement(_)
        | Statement::FunctionDeclaration(_)
        | Statement::ClassDeclaration(_)
        | Statement::TSModuleDeclaration(_) => push_forbidden_top_level_stmt(profile, errors),

        Statement::TSTypeAliasDeclaration(_) | Statement::TSInterfaceDeclaration(_) => {}

        _ => push_forbidden_top_level_stmt(profile, errors),
    }
}

fn push_forbidden_top_level_stmt(profile: UnitProfile, errors: &mut Vec<String>) {
    let kind = match profile {
        UnitProfile::Common => "common",
        UnitProfile::Api => "api",
        _ => "executable",
    };
    errors.push(format!(
        "Module body contains a forbidden top-level statement for {kind} units; use only imports and exports"
    ));
}

fn check_variable_declaration(
    var: &VariableDeclaration,
    profile: UnitProfile,
    errors: &mut Vec<String>,
) {
    if var.kind != VariableDeclarationKind::Const {
        errors.push(
            "Module-level bindings must use `const` (not `let`/`var`) so load-time state stays immutable"
                .to_string(),
        );
    }
    for declarator in &var.declarations {
        check_module_const_init(declarator, profile, errors);
    }
}

fn check_module_const_init(
    declarator: &VariableDeclarator,
    profile: UnitProfile,
    errors: &mut Vec<String>,
) {
    let Some(init) = &declarator.init else {
        errors.push("Module-level `const` must have an initializer".to_string());
        return;
    };
    if matches!(profile, UnitProfile::Workflow) && contains_nondeterministic_expr(init) {
        errors.push(
            "Workflow unit must not use `Date.now()`, `Math.random()`, or similar at module scope"
                .to_string(),
        );
        return;
    }
    if !is_pure_module_init_expr(init, profile) {
        errors.push(
            "Module-level `const` initializer must be a pure value (literals, `as const`, simple \
             operators, identifiers, or plain object/array literals) — no calls, `new`, `await`, or `sdk`"
                .to_string(),
        );
    }
}

fn check_export_default_factory(
    declaration: &ExportDefaultDeclarationKind,
    profile: UnitProfile,
    errors: &mut Vec<String>,
) {
    let ExportDefaultDeclarationKind::CallExpression(call) = declaration else {
        errors.push(
            "Executable unit must use `export default workflow(…)` / `actor(…)` / `queueWorker(…)`"
                .to_string(),
        );
        return;
    };

    let expected = match profile {
        UnitProfile::Workflow => "workflow",
        UnitProfile::Actor => "actor",
        UnitProfile::QueueWorker => "queueWorker",
        _ => return,
    };

    let Some(callee_name) = call_callee_name(&call.callee) else {
        errors.push(format!(
            "Executable unit `export default` must call `{expected}(…)` directly"
        ));
        return;
    };
    if callee_name != expected {
        errors.push(format!(
            "Executable unit `export default` must call `{expected}(…)`, not `{callee_name}(…)`"
        ));
    }

    for arg in &call.arguments {
        check_factory_argument(arg, profile, errors);
    }
}

fn check_factory_argument(arg: &Argument, profile: UnitProfile, errors: &mut Vec<String>) {
    match arg {
        Argument::SpreadElement(_) => {
            errors.push("Module body must not use spread arguments in the default export factory call"
                .to_string());
        }
        _ => {
            let expr = arg.to_expression();
            check_module_init_value_expr(expr, profile, errors);
        }
    }
}

fn check_module_init_value_expr(expr: &Expression, profile: UnitProfile, errors: &mut Vec<String>) {
    match peel_expression(expr) {
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => {}
        Expression::ObjectExpression(obj) => {
            for prop in &obj.properties {
                if let ObjectPropertyKind::ObjectProperty(p) = prop {
                    check_module_init_value_expr(&p.value, profile, errors);
                }
            }
        }
        Expression::ArrayExpression(arr) => {
            for el in &arr.elements {
                if let Some(expr) = el.as_expression() {
                    check_module_init_value_expr(expr, profile, errors);
                }
            }
        }
        other => {
            if !is_pure_module_init_expr(other, profile) {
                errors.push(
                    "Factory argument values (outside handler/run function bodies) must be pure — \
                     no calls, `new`, `await`, or `sdk` at module init"
                        .to_string(),
                );
            }
        }
    }
}

fn is_pure_module_init_expr(expr: &Expression, profile: UnitProfile) -> bool {
    match peel_expression(expr) {
        Expression::StringLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_) => true,

        Expression::TemplateLiteral(tpl) => tpl.expressions.is_empty(),

        Expression::UnaryExpression(unary) => {
            is_pure_module_init_expr(&unary.argument, profile)
        }

        Expression::BinaryExpression(bin) => {
            is_pure_module_init_expr(&bin.left, profile)
                && is_pure_module_init_expr(&bin.right, profile)
        }

        Expression::LogicalExpression(log) => {
            is_pure_module_init_expr(&log.left, profile)
                && is_pure_module_init_expr(&log.right, profile)
        }

        Expression::ConditionalExpression(cond) => {
            is_pure_module_init_expr(&cond.test, profile)
                && is_pure_module_init_expr(&cond.consequent, profile)
                && is_pure_module_init_expr(&cond.alternate, profile)
        }

        Expression::Identifier(ident) => ident.name.as_str() != "sdk",

        Expression::ArrayExpression(arr) => arr.elements.iter().all(|el| {
            el.as_expression()
                .is_some_and(|e| is_pure_module_init_expr(e, profile))
        }),

        Expression::ObjectExpression(obj) => obj.properties.iter().all(|prop| {
            if let ObjectPropertyKind::ObjectProperty(p) = prop {
                if matches!(&p.key, PropertyKey::PrivateIdentifier(_)) {
                    return false;
                }
                is_pure_module_init_expr(&p.value, profile)
            } else {
                false
            }
        }),

        Expression::ParenthesizedExpression(p) => is_pure_module_init_expr(&p.expression, profile),

        Expression::TSAsExpression(ts) => is_pure_module_init_expr(&ts.expression, profile),
        Expression::TSSatisfiesExpression(ts) => is_pure_module_init_expr(&ts.expression, profile),
        Expression::TSTypeAssertion(ts) => is_pure_module_init_expr(&ts.expression, profile),
        Expression::TSNonNullExpression(ts) => is_pure_module_init_expr(&ts.expression, profile),

        Expression::MetaProperty(_) => false,

        Expression::AwaitExpression(_) => false,

        Expression::CallExpression(_) => false,

        Expression::NewExpression(_) => false,

        Expression::ArrowFunctionExpression(_)
        | Expression::FunctionExpression(_)
        | Expression::ClassExpression(_)
        | Expression::TaggedTemplateExpression(_)
        | Expression::UpdateExpression(_)
        | Expression::AssignmentExpression(_)
        | Expression::YieldExpression(_)
        | Expression::ImportExpression(_)
        | Expression::Super(_)
        | Expression::SequenceExpression(_)
        | Expression::V8IntrinsicExpression(_) => false,

        Expression::StaticMemberExpression(_)
        | Expression::ComputedMemberExpression(_)
        | Expression::PrivateFieldExpression(_) => false,

        Expression::JSXElement(_)
        | Expression::JSXFragment(_)
        | Expression::TSInstantiationExpression(_)
        | Expression::ChainExpression(_)
        | Expression::ThisExpression(_)
        | Expression::PrivateInExpression(_) => false,
    }
}

fn contains_nondeterministic_expr(expr: &Expression) -> bool {
    match peel_expression(expr) {
        Expression::CallExpression(call) if is_nondeterministic_call(call) => true,
        Expression::ArrayExpression(arr) => arr
            .elements
            .iter()
            .filter_map(|el| el.as_expression())
            .any(contains_nondeterministic_expr),
        Expression::ObjectExpression(obj) => obj.properties.iter().any(|prop| {
            if let ObjectPropertyKind::ObjectProperty(p) = prop {
                contains_nondeterministic_expr(&p.value)
            } else {
                false
            }
        }),
        Expression::UnaryExpression(u) => contains_nondeterministic_expr(&u.argument),
        Expression::BinaryExpression(b) => {
            contains_nondeterministic_expr(&b.left) || contains_nondeterministic_expr(&b.right)
        }
        Expression::ParenthesizedExpression(p) => contains_nondeterministic_expr(&p.expression),
        Expression::TSAsExpression(ts) => contains_nondeterministic_expr(&ts.expression),
        Expression::TSSatisfiesExpression(ts) => contains_nondeterministic_expr(&ts.expression),
        Expression::TSTypeAssertion(ts) => contains_nondeterministic_expr(&ts.expression),
        Expression::TSNonNullExpression(ts) => contains_nondeterministic_expr(&ts.expression),
        _ => false,
    }
}

fn is_nondeterministic_call(call: &CallExpression) -> bool {
    let Expression::StaticMemberExpression(mem) = peel_expression(&call.callee) else {
        return false;
    };
    let Expression::Identifier(obj) = peel_expression(&mem.object) else {
        return false;
    };
    let prop = mem.property.name.as_str();
    matches!(
        (obj.name.as_str(), prop),
        ("Date", "now") | ("Math", "random")
    )
}

fn call_callee_name<'a>(callee: &'a Expression<'a>) -> Option<&'a str> {
    let Expression::Identifier(id) = peel_expression(callee) else {
        return None;
    };
    Some(id.name.as_str())
}

fn peel_expression<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
    match expr {
        Expression::ParenthesizedExpression(p) => peel_expression(&p.expression),
        Expression::TSAsExpression(ts) => peel_expression(&ts.expression),
        Expression::TSSatisfiesExpression(ts) => peel_expression(&ts.expression),
        Expression::TSTypeAssertion(ts) => peel_expression(&ts.expression),
        Expression::TSNonNullExpression(ts) => peel_expression(&ts.expression),
        _ => expr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(unit_type: &str, code: &str) -> Vec<String> {
        run_check(unit_type, code)
    }

    fn assert_ok(unit_type: &str, code: &str) {
        let errs = check(unit_type, code);
        assert!(errs.is_empty(), "{errs:?}");
    }

    fn assert_err(unit_type: &str, code: &str, needle: &str) {
        let errs = check(unit_type, code);
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "expected error containing {needle:?}, got {errs:?}"
        );
    }

    const API_TYPES: &str = r#"
export type RequestBody = never;
export type ResponseBody = { ok: boolean };
export type QueryParams = never;
"#;

    const WF_TYPES: &str = r#"
export type Input = void;
export type Output = void;
"#;

    // ── run_check entry ───────────────────────────────────────────────────────

    #[test]
    fn unknown_unit_type_is_skipped() {
        assert_ok("not_a_unit", "sdk.foo();");
    }

    #[test]
    fn empty_code_is_skipped() {
        assert_ok("api", "");
    }

    #[test]
    fn unparseable_code_returns_no_errors() {
        assert!(check("api", "export const x = ;").is_empty());
    }

    // ── common ──────────────────────────────────────────────────────────────

    #[test]
    fn common_allows_imports_and_named_exports() {
        assert_ok(
            "common",
            r#"
import type { Foo } from "@units/ctx.mod.common.other";
export function helper(): number { return 1; }
export type Row = { id: string };
export const MAX = 10;
"#,
        );
    }

    #[test]
    fn common_rejects_top_level_sdk() {
        assert_err(
            "common",
            r#"
import { sdk } from "@sdk";
sdk.foo();
export function f() {}
"#,
            "expression statements",
        );
    }

    #[test]
    fn common_rejects_top_level_const() {
        assert_err(
            "common",
            r#"
const SECRET = "x";
export function f() {}
"#,
            "must not declare top-level",
        );
    }

    #[test]
    fn common_rejects_export_default() {
        assert_err(
            "common",
            r#"
export default function f() {}
"#,
            "export default",
        );
    }

    #[test]
    fn common_rejects_export_star() {
        assert_err(
            "common",
            r#"
export * from "./other";
"#,
            "export * from",
        );
    }

    #[test]
    fn common_rejects_export_const_with_call() {
        assert_err(
            "common",
            r#"
export const cache = buildCache();
export function f() {}
"#,
            "pure value",
        );
    }

    #[test]
    fn common_rejects_export_const_sdk_identifier() {
        assert_err(
            "common",
            r#"
import { sdk } from "@sdk";
export const x = sdk;
"#,
            "pure value",
        );
    }

    #[test]
    fn common_rejects_forbidden_top_level_if() {
        assert_err(
            "common",
            r#"
if (true) { export function f() {} }
"#,
            "forbidden top-level statement",
        );
    }

    #[test]
    fn common_rejects_top_level_function_declaration() {
        assert_err(
            "common",
            r#"
function f() {}
"#,
            "forbidden top-level statement",
        );
    }

    #[test]
    fn common_rejects_export_enum_declaration() {
        assert_err(
            "common",
            r#"
export enum Status { Active, Done }
"#,
            "forbidden top-level statement",
        );
    }

    // ── api ───────────────────────────────────────────────────────────────────

    #[test]
    fn api_allows_const_unit_path() {
        assert_ok(
            "api",
            &format!(
                r#"
import type {{ IncomingMessage, ServerResponse }} from "node:http";
const WF = "ctx.mod.workflow.foo" as const;
{API_TYPES}
export async function handleRequest(_req: IncomingMessage, res: ServerResponse): Promise<void> {{
  res.end("ok");
}}
"#
            ),
        );
    }

    #[test]
    fn api_allows_pure_const_shapes() {
        assert_ok(
            "api",
            &format!(
                r#"
{API_TYPES}
const A = 1;
const B = "x";
const C = `static`;
const D = {{ a: 1, b: [true, null] }};
const E = (1 + 2) as const;
const F = true ? "a" : "b";
export async function handleRequest(): Promise<void> {{}}
"#
            ),
        );
    }

    #[test]
    fn api_allows_sdk_inside_handle_request() {
        assert_ok(
            "api",
            &format!(
                r#"
import {{ sdk }} from "@sdk";
{API_TYPES}
export async function handleRequest(): Promise<void> {{
  sdk.startWorkflowSync({{ unitPath: "a.b.workflow.c", input: {{}} }});
}}
"#
            ),
        );
    }

    #[test]
    fn api_rejects_top_level_await() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
const x = await Promise.resolve(1);
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "pure value",
        );
    }

    #[test]
    fn api_rejects_top_level_new() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
const client = new Map();
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "pure value",
        );
    }

    #[test]
    fn api_rejects_const_call() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
const x = parseInt("1");
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "pure value",
        );
    }

    #[test]
    fn api_rejects_const_sdk_identifier() {
        assert_err(
            "api",
            &format!(
                r#"
import {{ sdk }} from "@sdk";
{API_TYPES}
const x = sdk;
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "pure value",
        );
    }

    #[test]
    fn api_rejects_dynamic_template_literal() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
const x = `v=${{Date.now()}}`;
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "pure value",
        );
    }

    #[test]
    fn api_rejects_let_at_module_scope() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
let x = 1;
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "must use `const`",
        );
    }

    #[test]
    fn api_rejects_module_const_without_initializer() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let mut ret = Parser::new(&allocator, "const x = 1;", source_type).parse();
        assert!(ret.errors.is_empty());
        let Statement::VariableDeclaration(var) = &mut ret.program.body[0] else {
            panic!("expected var decl");
        };
        var.declarations[0].init = None;
        let mut errors = Vec::new();
        check_module_const_init(&var.declarations[0], UnitProfile::Api, &mut errors);
        assert!(errors.iter().any(|e| e.contains("must have an initializer")));
    }

    #[test]
    fn api_rejects_export_default() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
export default async function handleRequest(): Promise<void> {{}}
"#
            ),
            "export default",
        );
    }

    #[test]
    fn api_rejects_export_star() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
export * from "./other";
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "export * from",
        );
    }

    #[test]
    fn api_rejects_top_level_iife() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
(function () {{}})();
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "expression statements",
        );
    }

    #[test]
    fn api_rejects_top_level_for_loop() {
        assert_err(
            "api",
            &format!(
                r#"
{API_TYPES}
for (let i = 0; i < 1; i++) {{}}
export async function handleRequest(): Promise<void> {{}}
"#
            ),
            "forbidden top-level statement",
        );
    }

    // ── workflow ──────────────────────────────────────────────────────────────

    #[test]
    fn workflow_allows_sdk_inside_run() {
        assert_ok(
            "workflow",
            &format!(
                r#"
import {{ workflow, sdk }} from "@sdk";
{WF_TYPES}
export default workflow(async (ctx) => {{
  sdk.startWorkflowSync({{ unitPath: "a.b.workflow.c", input: {{}} }});
}});
"#
            ),
        );
    }

    #[test]
    fn workflow_allows_object_form_with_handlers() {
        assert_ok(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
export default workflow({{
  run: async (ctx, _input) => {{ await ctx.sleep(1); }},
  handlers: {{
    approve: async (ctx, _input) => {{ return {{ ok: true }}; }},
  }},
}});
"#
            ),
        );
    }

    #[test]
    fn workflow_allows_module_const_unit_path() {
        assert_ok(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
const CHILD = "ctx.mod.workflow.child" as const;
export default workflow(async () => ({{ path: CHILD }}));
"#
            ),
        );
    }

    #[test]
    fn workflow_rejects_date_now_at_module_scope() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
const t = Date.now();
export default workflow(async () => ({{ t }}));
"#
            ),
            "Date.now()",
        );
    }

    #[test]
    fn workflow_rejects_math_random_at_module_scope() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
const r = Math.random();
export default workflow(async () => ({{ r }}));
"#
            ),
            "Math.random()",
        );
    }

    #[test]
    fn workflow_rejects_date_now_nested_in_object_const() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
const cfg = {{ t: Date.now() }};
export default workflow(async () => ({{ cfg }}));
"#
            ),
            "Date.now()",
        );
    }

    #[test]
    fn workflow_rejects_top_level_sdk() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow, sdk }} from "@sdk";
{WF_TYPES}
sdk.startWorkflowSync({{ unitPath: "a.b.workflow.c", input: {{}} }});
export default workflow(async () => ({{}}));
"#
            ),
            "expression statements",
        );
    }

    #[test]
    fn workflow_rejects_wrong_factory_callee() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ actor }} from "@sdk";
{WF_TYPES}
export default actor({{ state: {{}}, handlers: {{}} }});
"#
            ),
            "must call `workflow(…)`, not `actor(…)`",
        );
    }

    #[test]
    fn workflow_rejects_export_default_non_call() {
        assert_err(
            "workflow",
            &format!(
                r#"
{WF_TYPES}
export default async function run() {{}}
"#
            ),
            "export default workflow(…)",
        );
    }

    #[test]
    fn workflow_rejects_spread_in_factory_call_arguments() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
const extra = [async () => ({{}})];
export default workflow(...extra);
"#
            ),
            "spread arguments",
        );
    }

    #[test]
    fn workflow_rejects_impure_state_in_factory_object() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
export default workflow({{
  run: async () => ({{}}),
  meta: buildMeta(),
}});
"#
            ),
            "Factory argument values",
        );
    }

    #[test]
    fn workflow_rejects_non_identifier_factory_callee() {
        assert_err(
            "workflow",
            &format!(
                r#"
import {{ workflow }} from "@sdk";
{WF_TYPES}
const factory = workflow;
export default factory(async () => ({{}}));
"#
            ),
            "not `factory(…)`",
        );
    }

    // ── actor ─────────────────────────────────────────────────────────────────

    #[test]
    fn actor_allows_pure_state_and_handlers() {
        assert_ok(
            "actor",
            &format!(
                r#"
import {{ actor }} from "@sdk";
export type State = {{ count: number }};
export default actor<State>({{
  state: {{ count: 0 }},
  handlers: {{
    inc: async (ctx) => {{ ctx.state.count += 1; }},
  }},
}});
"#
            ),
        );
    }

    #[test]
    fn actor_rejects_wrong_factory() {
        assert_err(
            "actor",
            r#"
import { workflow } from "@sdk";
export default workflow(async () => ({}));
"#,
            "must call `actor(…)`, not `workflow(…)`",
        );
    }

    #[test]
    fn actor_rejects_impure_initial_state() {
        assert_err(
            "actor",
            r#"
import { actor } from "@sdk";
export type State = { n: number };
export default actor<State>({
  state: { n: Date.now() },
  handlers: { noop: async () => {} },
});
"#,
            "Factory argument values",
        );
    }

    // ── queue_worker ──────────────────────────────────────────────────────────

    #[test]
    fn queue_worker_allows_arrow_process() {
        assert_ok(
            "queue_worker",
            r#"
import { queueWorker } from "@sdk";
export type Input = { id: string };
export default queueWorker<Input, void>(async (_ctx, data) => {
  void data.id;
});
"#,
        );
    }

    #[test]
    fn queue_worker_allows_object_form() {
        assert_ok(
            "queue_worker",
            r#"
import { queueWorker } from "@sdk";
export type Input = void;
export default queueWorker({
  process: async () => {},
});
"#,
        );
    }

    #[test]
    fn queue_worker_rejects_wrong_factory() {
        assert_err(
            "queue_worker",
            r#"
import { workflow } from "@sdk";
export type Input = void;
export type Output = void;
export default workflow(async () => ({}));
"#,
            "must call `queueWorker(…)`, not `workflow(…)`",
        );
    }

    // ── peel / nondeterministic helpers ───────────────────────────────────────

    #[test]
    fn is_nondeterministic_call_detects_date_now_and_math_random() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let src = "const a = Date.now(); const b = Math.random();";
        let ret = Parser::new(&allocator, src, source_type).parse();
        assert!(ret.errors.is_empty());
        let stmt = &ret.program.body[0];
        let Statement::VariableDeclaration(var) = stmt else {
            panic!("expected var decl");
        };
        let init0 = var.declarations[0].init.as_ref().unwrap();
        let Expression::CallExpression(call0) = peel_expression(init0) else {
            panic!("expected call");
        };
        assert!(is_nondeterministic_call(call0));

        let stmt1 = &ret.program.body[1];
        let Statement::VariableDeclaration(var1) = stmt1 else {
            panic!("expected var decl");
        };
        let init1 = var1.declarations[0].init.as_ref().unwrap();
        let Expression::CallExpression(call1) = peel_expression(init1) else {
            panic!("expected call");
        };
        assert!(is_nondeterministic_call(call1));
    }

    #[test]
    fn is_nondeterministic_call_ignores_other_calls() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let ret = Parser::new(&allocator, "const x = parseInt('1');", source_type).parse();
        assert!(ret.errors.is_empty());
        let Statement::VariableDeclaration(var) = &ret.program.body[0] else {
            panic!("expected var decl");
        };
        let Expression::CallExpression(call) = var.declarations[0].init.as_ref().unwrap() else {
            panic!("expected call");
        };
        assert!(!is_nondeterministic_call(call));
    }

    #[test]
    fn contains_nondeterministic_expr_finds_nested_math_random() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let ret = Parser::new(
            &allocator,
            "const o = { inner: { r: Math.random() } };",
            source_type,
        )
        .parse();
        assert!(ret.errors.is_empty());
        let Statement::VariableDeclaration(var) = &ret.program.body[0] else {
            panic!("expected var decl");
        };
        let init = var.declarations[0].init.as_ref().unwrap();
        assert!(contains_nondeterministic_expr(init));
    }

    #[test]
    fn is_pure_rejects_object_method_shorthand() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let ret = Parser::new(&allocator, "const o = { m() { return 1; } };", source_type).parse();
        assert!(ret.errors.is_empty());
        let Statement::VariableDeclaration(var) = &ret.program.body[0] else {
            panic!("expected var decl");
        };
        let init = var.declarations[0].init.as_ref().unwrap();
        assert!(!is_pure_module_init_expr(init, UnitProfile::Api));
    }

    #[test]
    fn is_pure_rejects_static_member_expression() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let ret = Parser::new(&allocator, "const x = Object.freeze({});", source_type).parse();
        assert!(ret.errors.is_empty());
        let Statement::VariableDeclaration(var) = &ret.program.body[0] else {
            panic!("expected var decl");
        };
        let init = var.declarations[0].init.as_ref().unwrap();
        assert!(!is_pure_module_init_expr(init, UnitProfile::Api));
    }

    #[test]
    fn call_callee_name_reads_parenthesized_identifier() {
        let allocator = Allocator::default();
        let source_type = SourceType::ts().with_module(true);
        let ret = Parser::new(&allocator, "((workflow))(async () => {});", source_type).parse();
        assert!(ret.errors.is_empty());
        let Statement::ExpressionStatement(expr_stmt) = &ret.program.body[0] else {
            panic!("expected expr stmt");
        };
        let Expression::CallExpression(call) = &expr_stmt.expression else {
            panic!("expected call");
        };
        assert_eq!(call_callee_name(&call.callee), Some("workflow"));
    }
}
