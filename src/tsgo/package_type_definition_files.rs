use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

const TYPE_DEPS_JSON: &str = include_str!("runtime-project-tsc-type-deps.json");
const NODE_MODULES_PREFIX: &str = "node_modules/";

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
            let paths = entry
                .files
                .into_iter()
                .map(|p| strip_node_modules_prefix(&p))
                .collect();
            (name, paths)
        })
        .collect()
});

fn strip_node_modules_prefix(path: &str) -> String {
    path.strip_prefix(NODE_MODULES_PREFIX)
        .unwrap_or(path)
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageTypeDefinitionFiles {
    pub package: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResolvePackageTypeDefinitionFilesResult {
    pub package_type_definition_files: Vec<PackageTypeDefinitionFiles>,
    pub unknown_packages: Vec<String>,
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
