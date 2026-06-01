use crate::models::projects::ProjectUnitResponse;
use crate::tsgo::node_modules_files;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const VIRTUAL_ROOT: &str = "/unit";
pub const VIRTUAL_NODE_MODULES: &str = "/unit/node_modules";
pub const VIRTUAL_UNITS_MODULES: &str = "/unit/node_modules/@units";

pub fn build_unit_source_map(code: &str, units: &[ProjectUnitResponse]) -> HashMap<String, String> {
    let mut sources = HashMap::new();
    sources.insert(format!("{VIRTUAL_ROOT}/main.ts"), code.to_string());
    for unit in units {
        if unit.is_deleted {
            continue;
        }
        if let Some(ref unit_code) = unit.code {
            let unit_file = format!(
                "{}/{}.{}.{}.{}.ts",
                VIRTUAL_UNITS_MODULES,
                unit.context_slug,
                unit.module_slug,
                unit.unit_type.as_slug(),
                unit.unit_name_slug,
            );
            sources.insert(unit_file, unit_code.clone());
        }
    }
    sources
}

pub fn build_vfs(code: &str, units: &[ProjectUnitResponse]) -> HashMap<String, String> {
    let mut vfs = build_unit_source_map(code, units);

    let tsconfig = json!({
        "compilerOptions": {
            "target": "ES2023",
            "module": "ES2022",
            "moduleResolution": "bundler",
            "lib": ["ES2023"],

            "types": ["node"],
            "declaration": false,
            "sourceMap": true,
            "skipLibCheck": true,

            "strict": true,
            "noEmit": true,
            "noImplicitAny": true,
            "noImplicitOverride": true,
            "strictNullChecks": true,
            "strictFunctionTypes": true,
            "strictBindCallApply": true,
            "strictPropertyInitialization": true,
            "noImplicitThis": true,
            "alwaysStrict": true,
            "useUnknownInCatchVariables": true,
            "noUncheckedIndexedAccess": true,
            "exactOptionalPropertyTypes": true,

            "verbatimModuleSyntax": true,
            "noUncheckedSideEffectImports": true,
            "erasableSyntaxOnly": true,

            "noUnusedLocals": true,
            "noUnusedParameters": true,
            "noImplicitReturns": true,
            "noFallthroughCasesInSwitch": true,
            "noPropertyAccessFromIndexSignature": true,

            "paths": {
                "@units/*": ["./node_modules/@units/*"]
            }
        },
        "files": ["main.ts"]
    });
    vfs.insert(
        format!("{VIRTUAL_ROOT}/tsconfig.json"),
        tsconfig.to_string(),
    );
    vfs.insert(
        format!("{VIRTUAL_ROOT}/package.json"),
        json!({
            "name": "virtual-project",
            "type": "module"
        })
        .to_string(),
    );

    for (rel_path, content) in node_modules_files::NODE_MODULES_FILE_DATA {
        let full_path = format!("{VIRTUAL_NODE_MODULES}/{rel_path}");
        vfs.insert(full_path, content.to_string());
    }

    vfs
}

/// On-disk project for tsgolint headless: writes unit sources, symlinks `node_modules` to `src/tsgo/node_modules`.
#[cfg(feature = "unit_technical_reviewer")]
pub struct MaterializedUnitProject {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub main_ts: PathBuf,
}

#[cfg(feature = "unit_technical_reviewer")]
pub fn materialize_for_disk_lint(
    project_root: &Path,
    vfs: &HashMap<String, String>,
) -> Result<MaterializedUnitProject, String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = temp.path().to_path_buf();

    let node_modules_src = project_root.join("src/tsgo/node_modules");
    if !node_modules_src.is_dir() {
        return Err(format!(
            "node_modules not found at {}. Run `cd src/tsgo && npm install` from the repo root.",
            node_modules_src.display()
        ));
    }

    for (vfs_path, content) in vfs {
        let Some(rel) = vfs_path.strip_prefix("/unit/") else {
            continue;
        };
        if rel == "node_modules" || rel.starts_with("node_modules/") {
            continue;
        }
        let dest = root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&dest, content).map_err(|e| e.to_string())?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&node_modules_src, root.join("node_modules"))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    {
        return Err("Disk lint materialization requires a Unix platform".to_string());
    }

    let main_ts = root.join("main.ts");
    if !main_ts.is_file() {
        return Err("materialized project is missing main.ts".to_string());
    }

    Ok(MaterializedUnitProject {
        _temp: temp,
        root,
        main_ts,
    })
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let mut dir = parent.to_path_buf();
        loop {
            roots.push(dir.clone());
            if !dir.pop() {
                break;
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

pub fn find_project_root() -> Result<PathBuf, String> {
    for root in search_roots() {
        if root.join("src/tsgo/node_modules").is_dir() {
            return Ok(root);
        }
    }
    Err(
        "Project root not found (expected src/tsgo/node_modules). Run from repo root or install tsgo deps."
            .to_string(),
    )
}

pub fn resolve_repo_relative_file(rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    for root in search_roots() {
        let candidate = root.join(rel_path);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "File not found at `{rel}` (searched cwd and parents of the executable)"
    ))
}

pub fn find_tsgo_bin() -> Result<PathBuf, String> {
    let rel = tsgo_bin_rel_path()?;
    resolve_repo_relative_file(&rel).map_err(|_| {
        format!(
            "tsgo binary not found at {rel}. Run `cd src/tsgo && npm install` from the repo root."
        )
    })
}

pub fn find_tsgolint_bin() -> Result<PathBuf, String> {
    if let Ok(path_str) = std::env::var("OXLINT_TSGOLINT_PATH") {
        let p = PathBuf::from(&path_str);
        if p.is_file() {
            return Ok(p);
        }
        let nested = p.join("tsgolint");
        if nested.is_file() {
            return Ok(nested);
        }
        return Err(format!(
            "OXLINT_TSGOLINT_PATH is set but no tsgolint binary found at {}",
            p.display()
        ));
    }

    let rel = tsgolint_bin_rel_path()?;
    resolve_repo_relative_file(&rel).map_err(|_| {
        format!(
            "tsgolint binary not found at {rel}. Run `cd src/tsgo && npm install` from the repo root."
        )
    })
}

fn tsgo_bin_rel_path() -> Result<String, String> {
    let pkg = typescript_native_preview_pkg()?;
    Ok(format!("src/tsgo/node_modules/{pkg}/lib/tsgo"))
}

fn tsgolint_bin_rel_path() -> Result<String, String> {
    let pkg = tsgolint_platform_pkg()?;
    Ok(format!("src/tsgo/node_modules/{pkg}/tsgolint"))
}

fn typescript_native_preview_pkg() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("@typescript/native-preview-darwin-arm64"),
        ("macos", "x86_64") => Ok("@typescript/native-preview-darwin-x64"),
        ("linux", "x86_64") => Ok("@typescript/native-preview-linux-x64"),
        ("linux", "aarch64") => Ok("@typescript/native-preview-linux-arm64"),
        (os, arch) => Err(format!("Unsupported platform for tsgo: {os}-{arch}")),
    }
}

fn tsgolint_platform_pkg() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("@oxlint-tsgolint/darwin-arm64"),
        ("macos", "x86_64") => Ok("@oxlint-tsgolint/darwin-x64"),
        ("linux", "x86_64") => Ok("@oxlint-tsgolint/linux-x64"),
        ("linux", "aarch64") => Ok("@oxlint-tsgolint/linux-arm64"),
        (os, arch) => Err(format!("Unsupported platform for tsgolint: {os}-{arch}")),
    }
}
