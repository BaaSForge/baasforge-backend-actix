use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;   // Add at the top with other imports

// TODO: cache the fs files in memory, so we don't have to read them from the disk every time.

// NOTE: The panic is a tsgo bug when receiving null for a required file read;
// `tsgo error: panic: runtime error: invalid memory address or nil pointer dereference`

const VIRTUAL_ROOT: &str = "/tsgo-vcheck-baal";
const VIRTUAL_NODE_MODULES: &str = "/tsgo-vcheck-baal/node_modules";

pub async fn run_typescript_check(code: &str) -> Result<(), String> {
    let vfs = build_vfs(code);
    let mut client = TsgoIpcClient::spawn(vfs).await?;

    client.call("initialize", json!({})).await?;

    let tsconfig_path = format!("{VIRTUAL_ROOT}/tsconfig.json");
    let main_ts_path = format!("{VIRTUAL_ROOT}/main.ts");

    // 2. Snapshot Lifecycle
    let snap_resp = client.call("updateSnapshot", json!({
        "openProject": tsconfig_path,            // ✅ file path, not directory
        "fileChanges": { 
            "invalidateAll": true, 
            "changed": [], 
            "created": [tsconfig_path, main_ts_path],   // ✅ explicitly add both
            "deleted": [] 
        }
    })).await?;
    
    let snapshot = snap_resp["snapshot"].as_str().ok_or("No snapshot handle")?.to_string();

    // 3. Resolve Project
    let proj_resp = client.call("getDefaultProjectForFile", json!({
        "snapshot": snapshot,
        "file": main_ts_path
    })).await?;
    
    let project = proj_resp["id"].as_str()
        .ok_or_else(|| format!("No project id in response: {}", proj_resp))?;

    // 4. Get Diagnostics
    let diags = client.call("getSemanticDiagnostics", json!({
        "snapshot": snapshot,
        "project": project,
        "file": main_ts_path
    })).await?;

    let _ = client.call("release", json!({ "handle": snapshot })).await;

    if diags.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        Ok(())
    } else {
        Err(format!("Type errors found: {}", diags))
    }
}

// --- VFS Logic ---

fn build_vfs(code: &str) -> HashMap<String, String> {
    let mut vfs = HashMap::new();

    let tsconfig = json!({
        "compilerOptions": {
            "target": "ESNext",
            "module": "NodeNext",
            "moduleResolution": "NodeNext",
            "strict": true,
            "noEmit": true,
            "skipLibCheck": true,
            "types": ["node"],
            "lib": ["esnext"],  // explicitly use only esnext
        },
        "files": ["main.ts"]
    });
    vfs.insert(format!("{}/tsconfig.json", VIRTUAL_ROOT), tsconfig.to_string());

    vfs.insert(format!("{}/main.ts", VIRTUAL_ROOT), code.to_string());

    vfs.insert(format!("{}/package.json", VIRTUAL_ROOT), json!({
        "name": "virtual-project",
        "type": "module"
    }).to_string());

    vfs
}

// --- IPC Client with Callback Support ---
struct TsgoIpcClient {
    #[allow(dead_code)]
    child: tokio::process::Child,
    writer: BufWriter<tokio::process::ChildStdin>,
    reader: BufReader<tokio::process::ChildStdout>,
    vfs: HashMap<String, String>,
    next_id: u64,
}

impl TsgoIpcClient {
    async fn spawn(vfs: HashMap<String, String>) -> Result<Self, String> {
        let tsgo_bin = find_tsgo_bin()?;
        let mut child = tokio::process::Command::new(tsgo_bin)
            .args([
                "--api",
                "--async",
                "--callbacks=readFile,fileExists,directoryExists",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
    
        let writer = BufWriter::new(child.stdin.take().unwrap());
        let reader = BufReader::new(child.stdout.take().unwrap());
    
        Ok(Self { child, writer, reader, vfs, next_id: 1 })
    }    

    // ---------- Public API ----------
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.to_string();
        self.next_id += 1;

        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_message(&msg).await?;

        loop {
            let resp = self.read_message().await?;

            // Handle callbacks (requests from tsgo to us)
            if resp.get("method").is_some() {
                let cb_id = resp["id"].clone();
                let cb_method = resp["method"].as_str().unwrap();
                let result = self.handle_fs_callback(cb_method, &resp["params"]);
                self.write_message(&json!({
                    "jsonrpc": "2.0",
                    "id": cb_id,
                    "result": result,
                }))
                .await?;
                continue;
            }

            // Handle response to our own request
            if resp["id"].as_str() == Some(&id) {
                if let Some(err) = resp.get("error") {
                    return Err(format!(
                        "tsgo error: {} (code: {})",
                        err["message"].as_str().unwrap_or("unknown"),
                        err["code"]
                    ));
                }
                return Ok(resp["result"].clone());
            }
        }
    }

    fn handle_fs_callback(&self, method: &str, params: &Value) -> Value {
        eprintln!("🔧 FS CALLBACK: method={}, params={}", method, params);
    
        // Extract path: string or first element of array
        let requested_path = match params {
            Value::String(s) => Some(s.as_str()),
            Value::Array(arr) => arr.first().and_then(Value::as_str),
            _ => None,
        }.unwrap_or("");
    
        let full_path = if requested_path.starts_with('/') {
            requested_path.to_string()
        } else if requested_path.is_empty() {
            VIRTUAL_ROOT.to_string()
        } else {
            format!("{}/{}", VIRTUAL_ROOT, requested_path)
        };

        let real_path = if full_path.starts_with(VIRTUAL_NODE_MODULES) {
            let suffix = &full_path[VIRTUAL_NODE_MODULES.len()..];
            let mut real = get_real_node_modules_path();
            if !suffix.is_empty() {
                real.push(suffix.trim_start_matches('/'));
            }
            real
        } else {
            PathBuf::from(&full_path)
        };        

        let normalized = Path::new(&full_path).components().collect::<PathBuf>();
        let full_path = normalized.to_string_lossy().to_string();
    
        eprintln!("   → full path: {}", full_path);
    
        let is_dir = |p: &str| -> bool {
            if p == VIRTUAL_ROOT || p == format!("{}/", VIRTUAL_ROOT) {
                return true;
            }
            let with_slash = if p.ends_with('/') { p.to_string() } else { format!("{}/", p) };
            self.vfs.keys().any(|k| k.starts_with(&with_slash))
        };
    
        match method {
            "readFile" => {
                if is_dir(&full_path) {
                    eprintln!("📁 readFile on directory -> null");
                    return Value::Null;
                }

                // First check virtual VFS
                if let Some(content) = self.vfs.get(&full_path) {
                    eprintln!("✅ readFile from VFS OK: {} ({} bytes)", full_path, content.len());
                    return json!({ "content": content });
                }

                // Fallback to real filesystem
                if real_path.exists() && real_path.is_file() {
                    if let Ok(content) = fs::read_to_string(&real_path) {
                        return json!({ "content": content });
                    }
                }    
                Value::Null
            }
            "fileExists" => {
                let exists = self.vfs.contains_key(&full_path) || real_path.exists();
                json!(exists)
            }
            "directoryExists" => {
                let exists = if full_path == VIRTUAL_ROOT || full_path == format!("{}/", VIRTUAL_ROOT) {
                    true
                } else if full_path.starts_with("/tsgo-vcheck-baal/node_modules") {
                    real_path.exists() && real_path.is_dir()
                } else {
                    real_path.is_dir()
                };
                json!(exists)
            }
            _ => {
                eprintln!("⚠️ Unhandled callback: {}", method);
                Value::Null
            }
        }
    }

    // ---------- JSON-RPC IO ----------
    async fn write_message(&mut self, msg: &Value) -> Result<(), String> {
        let body = msg.to_string();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer
            .write_all(header.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.writer
            .write_all(body.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.writer.flush().await.map_err(|e| e.to_string())
    }

    async fn read_message(&mut self) -> Result<Value, String> {
        let mut len = 0;
        loop {
            let mut line = String::new();
            let bytes_read = self.reader
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if bytes_read == 0 {
                return Err("Connection closed by tsgo".to_string());
            }
            if line.trim().is_empty() {
                break;
            }
            if line.starts_with("Content-Length: ") {
                len = line["Content-Length: ".len()..]
                    .trim()
                    .parse()
                    .map_err(|_| "Invalid Content-Length")?;
            }
        }
        let mut body = vec![0u8; len];
        self.reader
            .read_exact(&mut body)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }
}

/// Simplified binary locator.
fn find_tsgo_bin() -> Result<String, String> {
    let rel = tsgo_bin_rel_path()?;
    
    // Check relative to current working directory
    let p = PathBuf::from(&rel);
    if p.exists() {
        return Ok(p.to_string_lossy().into_owned());
    }

    // Check relative to executable (for production)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe.parent() {
            let p = root.join(&rel);
            if p.exists() {
                return Ok(p.to_string_lossy().into_owned());
            }
        }
    }

    Err(format!("tsgo binary not found at {rel}. Run `cd tsgo && npm install`"))
}

/// Platform-specific binary path.
fn tsgo_bin_rel_path() -> Result<String, String> {
    let pkg = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "@typescript/native-preview-darwin-arm64",
        ("macos", "x86_64") => "@typescript/native-preview-darwin-x64",
        ("linux", "x86_64") => "@typescript/native-preview-linux-x64",
        ("linux", "aarch64") => "@typescript/native-preview-linux-arm64",
        (os, arch) => return Err(format!("Unsupported: {os}-{arch}")),
    };
    Ok(format!("tsgo/node_modules/{pkg}/lib/tsgo"))
}

fn get_real_node_modules_path() -> PathBuf {
    // Relative to current working directory where tsgo is installed
    let cwd = std::env::current_dir().unwrap();
    cwd.join("tsgo/node_modules")
}
