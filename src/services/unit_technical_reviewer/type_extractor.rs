// Deep TypeScript type extraction for unit metadata.
//
// Given a TypeScript source file and a VFS containing all `@units/*` dependencies,
// this module extracts named type declarations with all their transitive dependencies
// collected inline — but kept as separate `type X = …` declarations rather than
// merged/inlined.
//
// Example output for `RequestBody` that references an imported `UserType`:
//
//   type UserType = 'user' | 'admin';
//   type RequestBody = { userType: UserType; name: string };
//
// This format enables granular JSON schema generation later.
//
// The full OXC-based implementation is compiled only when the
// `unit_technical_reviewer` feature is enabled.  A no-op stub is compiled
// otherwise so that the shared library builds cleanly for the main binary too.

use crate::models::projects::ProjectUnitResponse;
use crate::services::units::api::metadata::ApiUnitMetadata;

// ── Feature-gated implementation ─────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use std::collections::{HashMap, HashSet};

#[cfg(feature = "unit_technical_reviewer")]
use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;

/// TypeScript built-in / utility type names that must not be resolved from the VFS.
#[cfg(feature = "unit_technical_reviewer")]
const BUILTIN_TYPES: &[&str] = &[
    "Array",
    "ReadonlyArray",
    "IterableIterator",
    "Iterator",
    "Iterable",
    "Generator",
    "AsyncGenerator",
    "AsyncIterator",
    "AsyncIterable",
    "Promise",
    "Set",
    "Map",
    "WeakSet",
    "WeakMap",
    "WeakRef",
    "Date",
    "Error",
    "RegExp",
    "Function",
    "Object",
    "Number",
    "String",
    "Boolean",
    "Symbol",
    "BigInt",
    "Record",
    "Partial",
    "Required",
    "Readonly",
    "Pick",
    "Omit",
    "Extract",
    "Exclude",
    "NonNullable",
    "ReturnType",
    "Parameters",
    "ConstructorParameters",
    "InstanceType",
    "ThisParameterType",
    "OmitThisParameter",
    "ThisType",
    "Awaited",
    "PropertyKey",
    "TemplateStringsArray",
];

// ── Per-file parsed information ───────────────────────────────────────────────

/// Information extracted from a single type alias declaration.
#[cfg(feature = "unit_technical_reviewer")]
#[derive(Clone)]
struct TypeInfo {
    /// Type parameter text including angle brackets, e.g. `<T extends string, U>`.
    type_params_text: String,
    /// The right-hand side of the alias, e.g. `{ a: string } | never`.
    annotation_text: String,
    /// Names of type parameters declared on this alias (excluded from ref resolution).
    type_param_names: Vec<String>,
}

/// Parsed information about a source file.
#[cfg(feature = "unit_technical_reviewer")]
struct FileInfo {
    /// Maps `local_name` → `(vfs_key, original_exported_name)` for `@units/*` imports only.
    imports: HashMap<String, (String, String)>,
    /// Maps type-alias name → TypeInfo (both exported and non-exported aliases).
    types: HashMap<String, TypeInfo>,
}

#[cfg(feature = "unit_technical_reviewer")]
const UNITS_PREFIX: &str = "/unit/node_modules/@units/";

#[cfg(feature = "unit_technical_reviewer")]
fn parse_file_info(source: &str) -> FileInfo {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::{Declaration, ImportDeclarationSpecifier, ModuleExportName, Statement};
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if !ret.errors.is_empty() {
        return FileInfo {
            imports: HashMap::new(),
            types: HashMap::new(),
        };
    }

    let mut imports: HashMap<String, (String, String)> = HashMap::new();
    let mut types: HashMap<String, TypeInfo> = HashMap::new();

    for stmt in &ret.program.body {
        match stmt {
            // import { Foo } from '@units/...' or import type { Foo as Bar } from '@units/...'
            Statement::ImportDeclaration(import_decl) => {
                let module = import_decl.source.value.as_str();
                if let Some(path) = module.strip_prefix("@units/") {
                    let vfs_key = format!("{}{}.ts", UNITS_PREFIX, path);
                    if let Some(ref specifiers) = import_decl.specifiers {
                        for spec in specifiers.iter() {
                            if let ImportDeclarationSpecifier::ImportSpecifier(s) = spec {
                                let original = match &s.imported {
                                    ModuleExportName::IdentifierName(id) => {
                                        id.name.as_str().to_string()
                                    }
                                    ModuleExportName::IdentifierReference(id) => {
                                        id.name.as_str().to_string()
                                    }
                                    ModuleExportName::StringLiteral(lit) => {
                                        lit.value.as_str().to_string()
                                    }
                                };
                                let local = s.local.name.as_str().to_string();
                                imports.insert(local, (vfs_key.clone(), original));
                            }
                        }
                    }
                }
            }
            // export type Foo = …
            Statement::ExportNamedDeclaration(export) => {
                if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                    types.insert(
                        alias.id.name.as_str().to_string(),
                        extract_type_info(alias, source),
                    );
                }
            }
            // type Foo = … (non-exported)
            Statement::TSTypeAliasDeclaration(alias) => {
                types.insert(
                    alias.id.name.as_str().to_string(),
                    extract_type_info(alias, source),
                );
            }
            _ => {}
        }
    }

    FileInfo { imports, types }
}

#[cfg(feature = "unit_technical_reviewer")]
fn extract_type_info(
    alias: &oxc_ast::ast::TSTypeAliasDeclaration,
    source: &str,
) -> TypeInfo {
    use oxc_span::GetSpan;

    let type_params_text = alias
        .type_parameters
        .as_ref()
        .map(|tp| source[tp.span.start as usize..tp.span.end as usize].to_string())
        .unwrap_or_default();

    let type_param_names: Vec<String> = alias
        .type_parameters
        .as_ref()
        .map(|tp| {
            tp.params
                .iter()
                .map(|p| p.name.name.as_str().to_string())
                .collect()
        })
        .unwrap_or_default();

    let ann_span = alias.type_annotation.span();
    let annotation_text =
        source[ann_span.start as usize..ann_span.end as usize].to_string();

    TypeInfo {
        type_params_text,
        annotation_text,
        type_param_names,
    }
}

// ── Type reference collection ─────────────────────────────────────────────────

/// Collect all user-defined `TSTypeReference` identifier names in the given annotation text.
///
/// Wraps the text in a dummy alias (`type _T = …`) so it parses as a full program, then walks
/// the AST collecting every `TSTypeReference` that is a simple identifier (not a qualified name).
/// Built-in TypeScript utility types are excluded.
#[cfg(feature = "unit_technical_reviewer")]
fn collect_refs_in_annotation(annotation_text: &str) -> HashSet<String> {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::{TSTypeName, TSTypeReference};
    use oxc_ast_visit::{walk, Visit};
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    let wrapper = format!("type _T = {};", annotation_text);
    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, &wrapper, source_type).parse();
    if !ret.errors.is_empty() {
        return HashSet::new();
    }

    struct RefCollector {
        names: HashSet<String>,
    }

    impl<'a> Visit<'a> for RefCollector {
        fn visit_ts_type_reference(&mut self, node: &TSTypeReference<'a>) {
            if let TSTypeName::IdentifierReference(id) = &node.type_name {
                let name = id.name.as_str().to_string();
                if !BUILTIN_TYPES.contains(&name.as_str()) {
                    self.names.insert(name);
                }
            }
            walk::walk_ts_type_reference(self, node);
        }
    }

    let mut collector = RefCollector {
        names: HashSet::new(),
    };
    collector.visit_program(&ret.program);
    collector.names
}

// ── Recursive collector ───────────────────────────────────────────────────────

/// Mutable state for the DFS type-dependency traversal.
#[cfg(feature = "unit_technical_reviewer")]
struct Collector {
    vfs: HashMap<String, String>,
    visited: HashSet<(String, String)>,
    output: Vec<String>,
    emitted_keys: HashSet<String>,
    key_to_first_display: HashMap<String, String>,
}

#[cfg(feature = "unit_technical_reviewer")]
impl Collector {
    fn new(vfs: HashMap<String, String>) -> Self {
        Self {
            vfs,
            visited: HashSet::new(),
            output: Vec::new(),
            emitted_keys: HashSet::new(),
            key_to_first_display: HashMap::new(),
        }
    }

    /// Recursively collect the declaration for `source_name` in `vfs_key` (empty = main file),
    /// using `display_name` in the emitted `type <display_name> = …` line.
    fn collect(
        &mut self,
        display_name: &str,
        source_name: &str,
        vfs_key: &str,
        source: &str,
    ) {
        let unique_key = format!("{}::{}", vfs_key, source_name);

        // Already processed? Possibly emit an alias if the display name differs.
        if self.visited.contains(&(vfs_key.to_string(), source_name.to_string())) {
            if let Some(first_name) = self.key_to_first_display.get(&unique_key)
                && first_name != display_name
            {
                let alias_key = format!("alias::{}", display_name);
                if !self.emitted_keys.contains(&alias_key) {
                    let alias_text = format!("type {} = {};", display_name, first_name);
                    self.output.push(alias_text);
                    self.emitted_keys.insert(alias_key);
                }
            }
            return;
        }

        let file_info = parse_file_info(source);
        let Some(type_info) = file_info.types.get(source_name).cloned() else {
            return;
        };

        let TypeInfo {
            type_params_text,
            annotation_text,
            type_param_names,
        } = type_info;

        // Mark as visited *before* recursing to handle cyclic type references.
        self.visited
            .insert((vfs_key.to_string(), source_name.to_string()));
        self.key_to_first_display
            .insert(unique_key.clone(), display_name.to_string());

        // Collect type references, excluding this alias's own type parameters.
        let refs: Vec<String> = collect_refs_in_annotation(&annotation_text)
            .into_iter()
            .filter(|n| !type_param_names.contains(n))
            .collect();

        let imports = file_info.imports;
        let local_type_names: HashSet<String> = file_info.types.keys().cloned().collect();

        // Recurse: resolve all dependencies before emitting this type.
        for ref_name in &refs {
            if let Some((dep_vfs_key, dep_original)) = imports.get(ref_name.as_str()) {
                // The ref is imported from another @units file.
                if !self.visited.contains(&(dep_vfs_key.to_string(), dep_original.to_string())) {
                    if let Some(dep_source) = self.vfs.get(dep_vfs_key).cloned() {
                        let dep_vk = dep_vfs_key.clone();
                        let dep_orig = dep_original.clone();
                        // Display name = local alias used in this file.
                        self.collect(ref_name, &dep_orig, &dep_vk, &dep_source);
                    }
                } else {
                    // Already visited – emit alias if the local name differs.
                    let dep_uk = format!("{}::{}", dep_vfs_key, dep_original);
                    if let Some(first_name) = self.key_to_first_display.get(&dep_uk).cloned()
                        && first_name != *ref_name
                    {
                        let alias_key = format!("alias::{}", ref_name);
                        if !self.emitted_keys.contains(&alias_key) {
                            self.output
                                .push(format!("type {} = {};", ref_name, first_name));
                            self.emitted_keys.insert(alias_key);
                        }
                    }
                }
            } else if local_type_names.contains(ref_name.as_str()) {
                // The ref is a locally-defined alias in the same file.
                if !self.visited
                    .contains(&(vfs_key.to_string(), ref_name.to_string()))
                {
                    self.collect(ref_name, ref_name, vfs_key, source);
                }
            }
            // Otherwise: built-in, type parameter, or unresolvable – skip.
        }

        // Emit this type's declaration (post-order ensures dependencies come first).
        if !self.emitted_keys.contains(&unique_key) {
            let decl_text =
                format!("type {}{} = {};", display_name, type_params_text, annotation_text);
            self.output.push(decl_text);
            self.emitted_keys.insert(unique_key);
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
fn build_units_vfs(units: &[ProjectUnitResponse]) -> HashMap<String, String> {
    let mut vfs = HashMap::new();
    for unit in units {
        if unit.is_deleted {
            continue;
        }
        if let Some(ref code) = unit.code {
            let key = format!(
                "{}{}.{}.{}.{}.ts",
                UNITS_PREFIX,
                unit.context_slug,
                unit.module_slug,
                unit.unit_type.as_slug(),
                unit.unit_name_slug,
            );
            vfs.insert(key, code.clone());
        }
    }
    vfs
}

#[cfg(feature = "unit_technical_reviewer")]
fn extract_single_type(
    target_name: &str,
    code: &str,
    units_vfs: &HashMap<String, String>,
) -> Option<String> {
    let file_info = parse_file_info(code);
    if !file_info.types.contains_key(target_name) {
        return None;
    }

    let mut collector = Collector::new(units_vfs.clone());
    // Main file uses an empty string as its VFS key.
    collector.collect(target_name, target_name, "", code);

    if collector.output.is_empty() {
        None
    } else {
        Some(collector.output.join("\n"))
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Extract TypeScript type declarations for the three API-unit contract types.
///
/// Populates `request_body` (from `RequestBody`), `request_params` (from `QueryParams`),
/// and `response_body` (from `ResponseBody`).  The `method` field is left as `None`; callers
/// should set it separately.
///
/// JSON schema fields are intentionally left as empty strings – JSON schema generation is
/// handled in a later step.
///
/// Full implementation compiled only with the `unit_technical_reviewer` feature.
/// A no-op stub is compiled for all other targets.
#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_api_type_metadata(
    code: &str,
    units: &[ProjectUnitResponse],
) -> ApiUnitMetadata {
    let units_vfs = build_units_vfs(units);

    let pair = |ts: Option<String>| TypeScriptJsonSchemaPair {
        typescript: ts,
        json_schema: Some(String::new()),
    };

    ApiUnitMetadata {
        method: None,
        request_body: pair(extract_single_type("RequestBody", code, &units_vfs)),
        request_params: pair(extract_single_type("QueryParams", code, &units_vfs)),
        response_body: pair(extract_single_type("ResponseBody", code, &units_vfs)),
    }
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn extract_api_type_metadata(
    _code: &str,
    _units: &[ProjectUnitResponse],
) -> ApiUnitMetadata {
    ApiUnitMetadata::default()
}
