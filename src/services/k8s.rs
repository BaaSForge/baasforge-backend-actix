use crate::models::server_clusters::ServerCluster;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use kube::{
    Client, Config,
};
use ureq::http;

fn extract_der_from_pem(pem_str: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = pem_str.lines().collect();

    let mut in_certificate = false;
    let mut base64_lines = Vec::new();

    for line in lines {
        let line = line.trim();
        if line.contains("-----BEGIN CERTIFICATE-----") {
            in_certificate = true;
            continue;
        }
        if line.contains("-----END CERTIFICATE-----") {
            break;
        }
        if in_certificate && !line.is_empty() {
            base64_lines.push(line);
        }
    }

    if base64_lines.is_empty() {
        return Err("No certificate data found in PEM".into());
    }

    let base64_content = base64_lines.join("");
    BASE64.decode(&base64_content).map_err(|e| e.into())
}

#[derive(Clone)]
pub struct K8sClient {
    pub client: Client,
    pub namespace: String,
}

impl K8sClient {
    pub async fn from_server_cluster(
        server_cluster: &ServerCluster,
        namespace: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let metadata = server_cluster
            .metadata
            .as_ref()
            .ok_or("No metadata found in server cluster")?;

        let cluster_url_str = metadata
            .get("cluster-url")
            .and_then(|v| v.as_str())
            .ok_or("Missing or invalid 'cluster-url' in metadata")?;

        let token = metadata
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or("Missing or invalid 'token' in metadata")?;

        let ca_cert_b64 = metadata
            .get("cluster-cert-b64")
            .and_then(|v| v.as_str())
            .ok_or("Missing or invalid 'cluster-cert-b64' in metadata")?;

        let ca_cert_pem_bytes = BASE64.decode(ca_cert_b64)?;
        let ca_cert_pem = String::from_utf8_lossy(&ca_cert_pem_bytes);

        let root_cert = if ca_cert_pem.contains("-----BEGIN CERTIFICATE-----") {
            extract_der_from_pem(&ca_cert_pem)?
        } else {
            ca_cert_pem_bytes
        };

        let cluster_url = cluster_url_str
            .parse::<http::Uri>()
            .map_err(|e| format!("Failed to parse cluster URL as Uri: {}", e))?;

        let config = Config {
            cluster_url,
            default_namespace: "default".to_string(),
            root_cert: Some(vec![root_cert]),
            accept_invalid_certs: false,
            auth_info: kube::config::AuthInfo {
                token: Some(token.into()),
                ..Default::default()
            },
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            disable_compression: false,
            proxy_url: None,
            tls_server_name: None,
            headers: Vec::new(),
        };

        let client = Client::try_from(config)?;

        let namespace = namespace.unwrap_or_else(|| "default".to_string());

        Ok(Self { client, namespace })
    }
}
