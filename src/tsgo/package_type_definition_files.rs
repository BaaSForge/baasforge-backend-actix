use crate::tsgo::node_modules_files;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

const TYPE_DEPS_JSON: &str = include_str!("runtime-project-tsc-type-deps.json");
pub const NODE_MODULES_PREFIX: &str = "node_modules/";

#[derive(Debug, Deserialize)]
struct TypeDepsManifest {
    packages: HashMap<String, PackageEntry>,
}

#[derive(Debug, Deserialize)]
struct PackageEntry {
    files: Vec<String>,
}

static PACKAGE_FILE_PATHS: LazyLock<HashMap<String, Vec<String>>> = LazyLock::new(|| {
    let manifest: TypeDepsManifest =
        serde_json::from_str(TYPE_DEPS_JSON).expect("runtime-project-tsc-type-deps.json must parse");
    manifest
        .packages
        .into_iter()
        .map(|(name, entry)| {
            let paths: Vec<String> = entry
                .files
                .into_iter()
                .map(|p| {
                    if !p.starts_with(NODE_MODULES_PREFIX) {
                        panic!(
                            "{TYPE_DEPS_JSON} package `{name}` lists `{p}` without `{NODE_MODULES_PREFIX}` prefix"
                        );
                    }
                    p
                })
                .collect();
            (name, paths)
        })
        .collect()
});

static FILE_CONTENTS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    node_modules_files::NODE_MODULES_FILE_DATA
        .iter()
        .copied()
        .collect()
});

fn strip_node_modules_prefix(path: &str) -> String {
    path.strip_prefix(NODE_MODULES_PREFIX)
        .expect("path must start with node_modules/")
        .to_string()
}

pub fn validate_node_modules_file_paths(files: &[String]) -> Result<(), String> {
    let non_empty: Vec<&str> = files
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if non_empty.is_empty() {
        return Err("`files` must contain at least one non-empty file path.".to_string());
    }
    for path in non_empty {
        if !path.starts_with(NODE_MODULES_PREFIX) {
            return Err(format!(
                "File path `{path}` must start with `{NODE_MODULES_PREFIX}` (copy paths verbatim from `get_packages_type_definition_files`)."
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageTypeDefinitionFiles {
    pub package: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageTypeDefinitionFileContent {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResolvePackageTypeDefinitionFilesResult {
    pub package_type_definition_files: Vec<PackageTypeDefinitionFiles>,
    pub unknown_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResolvePackageTypeDefinitionFileContentsResult {
    pub package_type_definition_file_contents: Vec<PackageTypeDefinitionFileContent>,
    pub unknown_files: Vec<String>,
}

pub fn known_package_names() -> Vec<String> {
    let mut names: Vec<String> = PACKAGE_FILE_PATHS.keys().cloned().collect();
    names.sort();
    names
}

pub fn resolve_package_type_definition_files(
    packages: &[String],
) -> ResolvePackageTypeDefinitionFilesResult {
    let mut package_type_definition_files = Vec::new();
    let mut unknown_packages = Vec::new();

    for package in packages {
        let name = package.trim();
        if name.is_empty() {
            continue;
        }
        let Some(paths) = PACKAGE_FILE_PATHS.get(name) else {
            unknown_packages.push(name.to_string());
            continue;
        };
        package_type_definition_files.push(PackageTypeDefinitionFiles {
            package: name.to_string(),
            files: paths.clone(),
        });
    }

    unknown_packages.sort();
    unknown_packages.dedup();
    package_type_definition_files.sort_by(|a, b| a.package.cmp(&b.package));

    ResolvePackageTypeDefinitionFilesResult {
        package_type_definition_files,
        unknown_packages,
    }
}

pub fn resolve_package_type_definition_file_contents(
    files: &[String],
) -> ResolvePackageTypeDefinitionFileContentsResult {
    let mut package_type_definition_file_contents = Vec::new();
    let mut unknown_files = Vec::new();

    for file in files {
        let path = file.trim();
        if path.is_empty() {
            continue;
        }
        debug_assert!(path.starts_with(NODE_MODULES_PREFIX));
        let lookup_key = strip_node_modules_prefix(path);
        let Some(content) = FILE_CONTENTS.get(lookup_key.as_str()).copied() else {
            unknown_files.push(path.to_string());
            continue;
        };
        package_type_definition_file_contents.push(PackageTypeDefinitionFileContent {
            path: path.to_string(),
            content: content.to_string(),
        });
    }

    unknown_files.sort();
    unknown_files.dedup();

    ResolvePackageTypeDefinitionFileContentsResult {
        package_type_definition_file_contents,
        unknown_files,
    }
}
