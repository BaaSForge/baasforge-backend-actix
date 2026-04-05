use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let types_dir = PathBuf::from(&manifest_dir)
        .join("src/services/unit_technical_reviewer/@types/node");

    println!("cargo:rerun-if-changed={}", types_dir.display());

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = PathBuf::from(&out_dir).join("node_type_files.rs");

    let mut entries: Vec<(String, String)> = Vec::new();
    collect_dts_files(&types_dir, &types_dir, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut code =
        String::from("static NODE_TYPE_FILE_DATA: &[(&str, &str)] = &[\n");
    for (rel_path, abs_path) in &entries {
        code.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            rel_path, abs_path
        ));
    }
    code.push_str("];\n");

    fs::write(&out_path, code).unwrap();
}

fn collect_dts_files(base: &Path, dir: &Path, entries: &mut Vec<(String, String)>) {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dts_files(base, &path, entries);
        } else if path.extension().map(|e| e == "ts").unwrap_or(false) {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            entries.push((rel, path.to_str().unwrap().to_string()));
        }
    }
}
