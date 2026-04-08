// Generic deep-resolution TypeScript type extractor.
//
// Given a source file and a VFS of `@units/*` dependencies, resolves a named
// exported type alias and all its transitive dependencies into a sequence of
// separated `type X = …` declarations (dependencies first, target last).
//
// All emitted identifiers are **scoped** with a stable prefix derived from the
// originating unit's VFS path, e.g.:
//
//   "/unit/node_modules/@units/iimo.infra-test.command.unit-one.ts"
//   → prefix  "Iimo_Infra_Test_Command_Unit_One___"
//   → type UT → "Iimo_Infra_Test_Command_Unit_One___UT"
//
//   root file (vfs_key = "")  → prefix "Root___"
//
// Every type-reference identifier inside a body is also rewritten to the
// canonical scoped name of the type it resolves to, so `import { X as Y }` is
// treated correctly: `Y` in any body is replaced with the scoped name of `X`
// in its origin file.
//
// Cycle-safe: a `(vfs_key, source_name)` pair is inserted into `visited`
// *before* recursing.  A forward reference in a body is already written with
// the correct scoped name; TypeScript type declarations are order-independent,
// so mutually-recursive types are valid even if the dependency appears after
// its use in the output.
//
// The OXC-based implementation is compiled only when the
// `unit_technical_reviewer` feature is enabled.

use crate::models::projects::ProjectUnitResponse;

#[cfg(feature = "unit_technical_reviewer")]
use std::collections::{HashMap, HashSet};

// ── Constants ─────────────────────────────────────────────────────────────────

/// TypeScript built-in / utility type names that must not be resolved from the VFS.
#[cfg(feature = "unit_technical_reviewer")]
pub const BUILTIN_TYPES: &[&str] = &[
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

#[cfg(feature = "unit_technical_reviewer")]
const UNITS_PREFIX: &str = "/unit/node_modules/@units/";

// ── Per-file parsed information ───────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
#[derive(Clone)]
struct TypeInfo {
    type_params_text: String,
    annotation_text: String,
    type_param_names: Vec<String>,
}

#[cfg(feature = "unit_technical_reviewer")]
struct FileInfo {
    /// local_name → (vfs_key_of_origin, original_exported_name)
    imports: HashMap<String, (String, String)>,
    types: HashMap<String, TypeInfo>,
}

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
            Statement::ExportNamedDeclaration(export) => {
                if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                    types.insert(
                        alias.id.name.as_str().to_string(),
                        extract_type_info(alias, source),
                    );
                }
            }
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

/// Collect every non-builtin type-reference identifier in an annotation text.
/// Used to decide which dependencies to recurse into.
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

// ── Scoped-name helpers ───────────────────────────────────────────────────────

/// Derive a unique, stable identifier prefix from a VFS key.
///
/// `""` (root / current unit)
///   → `"Root___"`
///
/// `"/unit/node_modules/@units/iimo.infra-test.command.unit-one.ts"`
///   → `"Iimo_Infra_Test_Command_Unit_One___"`
///
/// The `___` suffix lets callers strip the prefix later with a simple split.
#[cfg(feature = "unit_technical_reviewer")]
fn make_prefix(vfs_key: &str) -> String {
    if vfs_key.is_empty() {
        return "Root___".to_string();
    }
    let slug = vfs_key
        .strip_prefix(UNITS_PREFIX)
        .unwrap_or(vfs_key)
        .strip_suffix(".ts")
        .unwrap_or(vfs_key);

    // Split on `.` and `-`, capitalise the first letter of each segment.
    let parts: Vec<String> = slug
        .split(|c: char| c == '.' || c == '-')
        .filter_map(|part| {
            let mut chars = part.chars();
            chars.next().map(|first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect();

    format!("{}___", parts.join("_"))
}

/// Return the globally-unique scoped identifier for a type in a given file.
///
/// E.g. `scoped_name("/unit/.../iimo.infra-test.command.unit-one.ts", "UT")`
///       → `"Iimo_Infra_Test_Command_Unit_One___UT"`
#[cfg(feature = "unit_technical_reviewer")]
fn scoped_name(vfs_key: &str, source_name: &str) -> String {
    format!("{}{}", make_prefix(vfs_key), source_name)
}

// ── Annotation rewriter ───────────────────────────────────────────────────────

/// Rewrite every type-reference identifier in `annotation_text` to its
/// canonical scoped name, using span-based replacement (reverse order so
/// offsets remain valid).
///
/// Resolution rules (in priority order):
///   1. Name is in `imports`  → `scoped_name(dep_vfs_key, dep_original_name)`
///   2. Name is a local type  → `scoped_name(vfs_key, name)`
///   3. Name is a type param or builtin → left unchanged
///   4. Unknown              → left unchanged
#[cfg(feature = "unit_technical_reviewer")]
fn rewrite_annotation(
    annotation_text: &str,
    vfs_key: &str,
    type_param_names: &[String],
    imports: &HashMap<String, (String, String)>,
    local_types: &HashSet<String>,
) -> String {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::{TSTypeName, TSTypeReference};
    use oxc_ast_visit::{walk, Visit};
    use oxc_parser::Parser;
    use oxc_span::{GetSpan, SourceType};

    // Wrap in a throwaway alias so the annotation parses as a full program.
    const WRAP_PREFIX: &str = "type _T = ";
    let wrapper = format!("{}{};", WRAP_PREFIX, annotation_text);
    let offset = WRAP_PREFIX.len() as u32;

    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let ret = Parser::new(&allocator, &wrapper, source_type).parse();
    if !ret.errors.is_empty() {
        return annotation_text.to_string();
    }

    // Collect (start, end, name) spans from the wrapper string.
    struct SpanCollector {
        spans: Vec<(u32, u32, String)>,
        type_param_names: Vec<String>,
    }

    impl<'a> Visit<'a> for SpanCollector {
        fn visit_ts_type_reference(&mut self, node: &TSTypeReference<'a>) {
            if let TSTypeName::IdentifierReference(id) = &node.type_name {
                let name = id.name.as_str().to_string();
                if !BUILTIN_TYPES.contains(&name.as_str())
                    && !self.type_param_names.contains(&name)
                {
                    let span = id.span();
                    self.spans.push((span.start, span.end, name));
                }
            }
            walk::walk_ts_type_reference(self, node);
        }
    }

    let mut span_col = SpanCollector {
        spans: Vec::new(),
        type_param_names: type_param_names.to_vec(),
    };
    span_col.visit_program(&ret.program);

    // Process from end → start so earlier offsets stay valid after replacement.
    let mut spans = span_col.spans;
    spans.sort_by(|a, b| b.0.cmp(&a.0));

    let mut result = annotation_text.to_string();
    for (start, end, name) in spans {
        // Translate from wrapper coords to annotation_text coords.
        if start < offset {
            continue;
        }
        let ann_start = (start - offset) as usize;
        let ann_end = (end - offset) as usize;
        if ann_end > result.len() {
            continue;
        }

        let replacement = if let Some((dep_vfs_key, dep_orig)) = imports.get(&name) {
            // Import alias or direct import: resolve to the original's scoped name.
            scoped_name(dep_vfs_key, dep_orig)
        } else if local_types.contains(&name) {
            // Local type declaration in the same file.
            scoped_name(vfs_key, &name)
        } else {
            continue; // Type param / builtin / unknown — leave as-is.
        };

        result.replace_range(ann_start..ann_end, &replacement);
    }

    result
}

// ── Recursive collector ───────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
struct Collector {
    vfs: HashMap<String, String>,
    /// Tracks both in-progress and completed `(vfs_key, source_name)` pairs.
    /// Inserted *before* recursing so cycles are broken immediately.
    visited: HashSet<(String, String)>,
    output: Vec<String>,
    emitted_keys: HashSet<String>,
}

#[cfg(feature = "unit_technical_reviewer")]
impl Collector {
    fn new(vfs: HashMap<String, String>) -> Self {
        Self {
            vfs,
            visited: HashSet::new(),
            output: Vec::new(),
            emitted_keys: HashSet::new(),
        }
    }

    /// Resolve `source_name` (as it appears in `source`, living at `vfs_key`)
    /// and emit — in dependency-first order — uniquely scoped `type` declarations
    /// for it and all transitive dependencies.
    fn collect(&mut self, vfs_key: &str, source_name: &str, source: &str) {
        // Cycle / duplicate guard: `HashSet::insert` returns false if already present.
        if !self.visited.insert((vfs_key.to_string(), source_name.to_string())) {
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

        let local_types: HashSet<String> = file_info.types.keys().cloned().collect();

        // Recurse into transitive dependencies first (dependency order in output).
        let refs: Vec<String> = collect_refs_in_annotation(&annotation_text)
            .into_iter()
            .filter(|n| !type_param_names.contains(n))
            .collect();

        for ref_name in &refs {
            if let Some((dep_vfs_key, dep_orig)) = file_info.imports.get(ref_name.as_str()) {
                // Imported name — may be a direct import or an `import X as Y` alias.
                // Either way, `dep_orig` is the canonical name in the origin file.
                if let Some(dep_source) = self.vfs.get(dep_vfs_key).cloned() {
                    let dvk = dep_vfs_key.clone();
                    let do_ = dep_orig.clone();
                    self.collect(&dvk, &do_, &dep_source);
                }
            } else if local_types.contains(ref_name.as_str()) {
                // Local type in the same file.
                self.collect(vfs_key, ref_name, source);
            }
        }

        // Emit this type with its scoped identifier and the rewritten body.
        let emit_key = format!("{}::{}", vfs_key, source_name);
        if !self.emitted_keys.contains(&emit_key) {
            let my_scoped = scoped_name(vfs_key, source_name);
            let rewritten = rewrite_annotation(
                &annotation_text,
                vfs_key,
                &type_param_names,
                &file_info.imports,
                &local_types,
            );
            self.output.push(format!(
                "type {}{} = {};",
                my_scoped, type_params_text, rewritten
            ));
            self.emitted_keys.insert(emit_key);
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a VFS map `"/unit/node_modules/@units/<slug>.ts"` → source from a unit slice.
///
/// Exposed so per-unit type extractors can reuse it.
#[cfg(feature = "unit_technical_reviewer")]
pub fn build_units_vfs(units: &[ProjectUnitResponse]) -> HashMap<String, String> {
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

/// Extract the deeply-resolved TypeScript declaration string for a single named exported type.
///
/// Returns `type` declarations joined by newlines, in dependency-first order.
/// All identifiers are scoped with a file-derived prefix so that identically-named
/// types across different units never produce `Duplicate identifier` errors.
///
/// Prefix format: `<PascalSegments>___` derived from the unit's VFS path; the
/// three-underscore separator lets callers strip it with a simple split.
/// The current unit's own types use the `Root___` prefix.
///
/// Returns `None` if `target_name` is not an exported type in `code`.
#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_single_type(
    target_name: &str,
    code: &str,
    units_vfs: &HashMap<String, String>,
) -> Option<String> {
    let file_info = parse_file_info(code);
    if !file_info.types.contains_key(target_name) {
        return None;
    }
    let mut collector = Collector::new(units_vfs.clone());
    collector.collect("", target_name, code);
    if collector.output.is_empty() {
        None
    } else {
        Some(collector.output.join("\n"))
    }
}
