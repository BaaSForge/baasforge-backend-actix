//! TypeScript type-alias blocks (as emitted by `type_extractor`) → JSON Schema (draft 2020-12).
//!
//! There is no maintained Rust crate for arbitrary TS source → JSON Schema without executing
//! TypeScript. This uses the same OXC stack as `type_extractor`. Alias references become `$ref`
//! into `$defs`; built-ins and common utilities (`Array`, `Record`, `Partial`) are handled.

use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;

pub const JSON_SCHEMA_DRAFT_2020_12: &str =
    "https://json-schema.org/draft/2020-12/schema";

#[cfg(feature = "unit_technical_reviewer")]
use oxc_allocator::Allocator;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_ast::ast::{
    PropertyKey, Statement, TSLiteral, TSSignature, TSTupleElement, TSType, TSTypeName,
    TSTypeOperatorOperator,
};
#[cfg(feature = "unit_technical_reviewer")]
use oxc_parser::Parser;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_span::SourceType;
#[cfg(feature = "unit_technical_reviewer")]
use serde_json::{json, Map, Value};
#[cfg(feature = "unit_technical_reviewer")]
use std::collections::HashMap;

#[cfg(feature = "unit_technical_reviewer")]
fn last_type_alias_name(declarations: &str) -> Option<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, declarations, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }
    let mut last = None;
    for stmt in &ret.program.body {
        if let Statement::TSTypeAliasDeclaration(alias) = stmt {
            last = Some(alias.id.name.as_str().to_string());
        }
    }
    last
}

#[cfg(feature = "unit_technical_reviewer")]
pub fn typescript_declarations_to_json_schema(
    declarations: &str,
    root_type_name: &str,
) -> Option<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, declarations, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }
    let mut aliases: HashMap<&str, &TSType> = HashMap::new();
    for stmt in &ret.program.body {
        if let Statement::TSTypeAliasDeclaration(alias) = stmt {
            aliases.insert(alias.id.name.as_str(), &alias.type_annotation);
        }
    }
    if !aliases.contains_key(root_type_name) {
        return None;
    }
    let mut names: Vec<&str> = aliases.keys().copied().collect();
    names.sort_unstable();
    let mut defs = Map::new();
    for name in names {
        let sch = materialize(aliases[name], &aliases);
        defs.insert(name.to_string(), sch);
    }
    let mut doc = Map::new();
    doc.insert(
        "$schema".to_string(),
        Value::String(JSON_SCHEMA_DRAFT_2020_12.to_string()),
    );
    doc.insert(
        "$ref".to_string(),
        Value::String(format!("#/$defs/{root_type_name}")),
    );
    doc.insert("$defs".to_string(), Value::Object(defs));
    serde_json::to_string(&Value::Object(doc)).ok()
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn typescript_declarations_to_json_schema(
    _declarations: &str,
    _root_type_name: &str,
) -> Option<String> {
    None
}

pub fn enrich_type_script_json_schema_pair(
    pair: &mut TypeScriptJsonSchemaPair,
    root_type_name: Option<&str>,
) {
    #[cfg(feature = "unit_technical_reviewer")]
    if let Some(ts) = pair.typescript.as_deref()
        && !ts.trim().is_empty()
    {
        let root = root_type_name
            .map(|s| s.to_string())
            .or_else(|| last_type_alias_name(ts));
        if let Some(ref name) = root
            && let Some(schema) = typescript_declarations_to_json_schema(ts, name)
        {
            pair.json_schema = Some(schema);
        }
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn materialize(ty: &TSType, aliases: &HashMap<&str, &TSType>) -> Value {
    match ty {
        TSType::TSParenthesizedType(p) => {
            materialize(&p.type_annotation, aliases)
        }
        TSType::TSAnyKeyword(_) | TSType::TSUnknownKeyword(_) => json!({}),
        TSType::TSNeverKeyword(_) => json!({ "not": {} }),
        TSType::TSStringKeyword(_) => json!({ "type": "string" }),
        TSType::TSNumberKeyword(_) => json!({ "type": "number" }),
        TSType::TSBooleanKeyword(_) => json!({ "type": "boolean" }),
        TSType::TSBigIntKeyword(_) => json!({ "type": "string", "pattern": "^-?[0-9]+$" }),
        TSType::TSNullKeyword(_) => json!({ "type": "null" }),
        TSType::TSUndefinedKeyword(_) | TSType::TSVoidKeyword(_) => json!({ "type": "null" }),
        TSType::TSObjectKeyword(_) => json!({ "type": "object" }),
        TSType::TSSymbolKeyword(_) => json!({ "type": "string" }),
        TSType::TSThisType(_) => json!({}),
        TSType::TSLiteralType(lit) => literal_schema(&lit.literal),
        TSType::TSArrayType(arr) => {
            json!({
                "type": "array",
                "items": materialize(&arr.element_type, aliases)
            })
        }
        TSType::TSUnionType(u) => {
            let parts: Vec<Value> = u
                .types
                .iter()
                .map(|t| materialize(t, aliases))
                .collect();
            if parts.len() == 1 {
                parts.into_iter().next().unwrap()
            } else {
                json!({ "anyOf": parts })
            }
        }
        TSType::TSIntersectionType(i) => {
            let parts: Vec<Value> = i
                .types
                .iter()
                .map(|t| materialize(t, aliases))
                .collect();
            json!({ "allOf": parts })
        }
        TSType::TSTypeOperatorType(op) if op.operator == TSTypeOperatorOperator::Readonly => {
            materialize(&op.type_annotation, aliases)
        }
        TSType::TSTypeOperatorType(_) => json!({}),
        TSType::TSTypeLiteral(obj) => object_literal_schema(obj, aliases),
        TSType::TSTupleType(tup) => tuple_schema(tup, aliases),
        TSType::TSTypeReference(tr) => {
            reference_schema(tr, aliases)
        }
        TSType::TSConditionalType(_)
        | TSType::TSConstructorType(_)
        | TSType::TSFunctionType(_)
        | TSType::TSImportType(_)
        | TSType::TSIndexedAccessType(_)
        | TSType::TSInferType(_)
        | TSType::TSMappedType(_)
        | TSType::TSNamedTupleMember(_)
        | TSType::TSTemplateLiteralType(_)
        | TSType::TSTypePredicate(_)
        | TSType::TSTypeQuery(_)
        | TSType::TSIntrinsicKeyword(_)
        | TSType::JSDocNullableType(_)
        | TSType::JSDocNonNullableType(_)
        | TSType::JSDocUnknownType(_) => json!({}),
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn literal_schema(lit: &TSLiteral) -> Value {
    match lit {
        TSLiteral::BooleanLiteral(b) => json!({ "const": b.value }),
        TSLiteral::NumericLiteral(n) => json!({ "const": n.value }),
        TSLiteral::StringLiteral(s) => json!({ "const": s.value.as_str() }),
        TSLiteral::BigIntLiteral(b) => json!({ "const": b.value.as_str() }),
        TSLiteral::TemplateLiteral(_) | TSLiteral::UnaryExpression(_) => json!({ "type": "string" }),
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn property_key_json_name(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str().to_string()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str().to_string()),
        _ => None,
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn object_literal_schema(obj: &oxc_ast::ast::TSTypeLiteral, aliases: &HashMap<&str, &TSType>) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    let mut additional: Option<Value> = None;

    for m in &obj.members {
        match m {
            TSSignature::TSPropertySignature(sig) => {
                let Some(name) = property_key_json_name(&sig.key) else {
                    continue;
                };
                let ann = sig
                    .type_annotation
                    .as_ref()
                    .map(|a| &a.type_annotation);
                let Some(inner_ty) = ann else {
                    continue;
                };
                let sch = if sig.optional {
                    let inner = materialize(inner_ty, aliases);
                    json!({ "anyOf": [inner, { "type": "null" }] })
                } else {
                    required.push(Value::String(name.clone()));
                    materialize(inner_ty, aliases)
                };
                props.insert(name, sch);
            }
            TSSignature::TSIndexSignature(idx) => {
                additional = Some(materialize(
                    &idx.type_annotation.type_annotation,
                    aliases,
                ));
            }
            _ => {}
        }
    }

    let mut out = Map::new();
    out.insert("type".into(), "object".into());
    out.insert("properties".into(), Value::Object(props));
    if !required.is_empty() {
        out.insert("required".into(), Value::Array(required));
    }
    if let Some(ap) = additional {
        out.insert("additionalProperties".into(), ap);
    } else {
        out.insert("additionalProperties".into(), Value::Bool(false));
    }
    Value::Object(out)
}

#[cfg(feature = "unit_technical_reviewer")]
fn tuple_schema(tup: &oxc_ast::ast::TSTupleType, aliases: &HashMap<&str, &TSType>) -> Value {
    let mut prefix = Vec::new();
    for el in &tup.element_types {
        let t = match el {
            TSTupleElement::TSOptionalType(o) => {
                let inner = materialize(&o.type_annotation, aliases);
                json!({ "anyOf": [inner, { "type": "null" }] })
            }
            TSTupleElement::TSRestType(r) => {
                json!({
                    "type": "array",
                    "items": materialize(&r.type_annotation, aliases)
                })
            }
            _ => {
                if let Some(inner) = el.as_ts_type() {
                    materialize(inner, aliases)
                } else {
                    json!({})
                }
            }
        };
        prefix.push(t);
    }
    json!({
        "type": "array",
        "prefixItems": prefix,
        "minItems": prefix.len(),
        "maxItems": prefix.len()
    })
}

#[cfg(feature = "unit_technical_reviewer")]
fn root_ident_from_type_name<'a>(name: &'a TSTypeName<'a>) -> Option<&'a str> {
    match name {
        TSTypeName::IdentifierReference(id) => Some(id.name.as_str()),
        TSTypeName::QualifiedName(qn) => Some(qn.right.name.as_str()),
        TSTypeName::ThisExpression(_) => None,
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn alias_ref_json(name: &str) -> Value {
    json!({ "$ref": format!("#/$defs/{name}") })
}

#[cfg(feature = "unit_technical_reviewer")]
fn reference_schema(tr: &oxc_ast::ast::TSTypeReference, aliases: &HashMap<&str, &TSType>) -> Value {
    let Some(base) = root_ident_from_type_name(&tr.type_name) else {
        return json!({});
    };

    let args = tr
        .type_arguments
        .as_ref()
        .map(|a| a.params.as_slice())
        .unwrap_or(&[]);

    match base {
        "Array" | "ReadonlyArray" => {
            if let Some(first) = args.first() {
                json!({
                    "type": "array",
                    "items": materialize(first, aliases)
                })
            } else {
                json!({ "type": "array" })
            }
        }
        "Promise" => args
            .first()
            .map(|t| materialize(t, aliases))
            .unwrap_or_else(|| json!({})),
        "Record" => {
            let val = args.get(1).or_else(|| args.first());
            if let Some(vt) = val {
                json!({
                    "type": "object",
                    "additionalProperties": materialize(vt, aliases)
                })
            } else {
                json!({ "type": "object" })
            }
        }
        "Partial" => {
            if let Some(inner) = args.first() {
                if let TSType::TSTypeLiteral(obj) = inner {
                    let mut props = Map::new();
                    for m in &obj.members {
                        if let TSSignature::TSPropertySignature(sig) = m {
                            let Some(name) = property_key_json_name(&sig.key) else {
                                continue;
                            };
                            let ann = sig
                                .type_annotation
                                .as_ref()
                                .map(|a| &a.type_annotation);
                            let Some(inner_ty) = ann else {
                                continue;
                            };
                            let sch = materialize(inner_ty, aliases);
                            let sch = json!({ "anyOf": [sch, { "type": "null" }] });
                            props.insert(name, sch);
                        }
                    }
                    return json!({
                        "type": "object",
                        "properties": props,
                        "additionalProperties": false
                    });
                }
                if let TSType::TSTypeReference(inner_tr) = inner {
                    if let Some(id) = root_ident_from_type_name(&inner_tr.type_name)
                        && aliases.contains_key(id)
                    {
                        return json!({
                            "allOf": [alias_ref_json(id), { "type": "object" }]
                        });
                    }
                }
            }
            json!({ "type": "object" })
        }
        "Pick" | "Omit" | "Required" | "Readonly" => args
            .first()
            .map(|t| materialize(t, aliases))
            .unwrap_or_else(|| json!({})),
        _ => {
            if aliases.contains_key(base) {
                return alias_ref_json(base);
            }
            match base {
                "String" => json!({ "type": "string" }),
                "Number" => json!({ "type": "number" }),
                "Boolean" => json!({ "type": "boolean" }),
                "BigInt" => json!({ "type": "string" }),
                "Object" => json!({ "type": "object" }),
                _ => json!({}),
            }
        }
    }
}

#[cfg(all(test, feature = "unit_technical_reviewer"))]
mod tests {
    use super::*;

    #[test]
    fn object_and_ref() {
        let ts = "type Root___A = { n: number };\ntype Root___B = { a: Root___A };";
        let s = typescript_declarations_to_json_schema(ts, "Root___B").unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert!(v.get("$defs").is_some());
        assert_eq!(v["$ref"].as_str(), Some("#/$defs/Root___B"));
    }
}
