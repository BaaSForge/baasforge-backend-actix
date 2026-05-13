use crate::services::unit_technical_reviewer::rule_checker::ast_utils::get_export_default_call_expression;
use oxc_ast::ast::{
    Expression, ObjectExpression, ObjectPropertyKind, Program, PropertyKey,
};

fn peel_parentheses<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
    let mut cur = expr;
    while let Expression::ParenthesizedExpression(p) = cur {
        cur = &p.expression;
    }
    cur
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

fn property_key_static_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

fn object_literal_static_property_value<'a>(
    obj: &'a ObjectExpression<'a>,
    prop_name: &str,
) -> Option<&'a Expression<'a>> {
    for item in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = item else {
            continue;
        };
        if prop.computed {
            continue;
        }
        let key = property_key_static_name(&prop.key)?;
        if key == prop_name {
            return Some(&prop.value);
        }
    }
    None
}

/// If the default export is `actor(…)` with a static config object:
/// - `None` — root argument is not a static object literal, or the call is not `actor`.
/// - `Some(None)` — `handlers` is omitted, or `handlers` is not a static object literal (no
///   validation; the actor may rely only on `onAlarm` / `onDeadLetter` / alarms).
/// - `Some(Some(keys))` — static `handlers: { … }`; `keys` may be empty (`handlers: {}`).
pub fn parse_actor_handlers_literal_keys<'a>(
    program: &'a Program<'a>,
) -> Option<Option<Vec<&'a str>>> {
    let call = get_export_default_call_expression(program)?;
    let callee = call_callee_expression(&call.callee);
    let Expression::Identifier(id) = callee else {
        return None;
    };
    if id.name.as_str() != "actor" {
        return None;
    }
    let arg0 = call.arguments.first()?;
    if arg0.is_spread() {
        return None;
    }
    let root_expr = peel_parentheses(arg0.as_expression()?);
    let Expression::ObjectExpression(root_obj) = root_expr else {
        return None;
    };
    let Some(handlers_expr) = object_literal_static_property_value(root_obj, "handlers") else {
        return Some(None);
    };
    let handlers_expr = peel_parentheses(handlers_expr);
    let Expression::ObjectExpression(handlers_obj) = handlers_expr else {
        return Some(None);
    };
    let mut keys = Vec::new();
    for item in &handlers_obj.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = item else {
            continue;
        };
        if prop.computed {
            continue;
        }
        if let Some(name) = property_key_static_name(&prop.key) {
            keys.push(name);
        }
    }
    Some(Some(keys))
}

pub fn actor_handler_key_to_exported_type_prefix(handler_key: &str) -> String {
    let mut it = handler_key.chars();
    let Some(first) = it.next() else {
        return String::new();
    };
    first.to_uppercase().chain(it).collect()
}
