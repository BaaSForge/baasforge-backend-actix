use crate::libs::api_response::api_error;
use crate::models::server_clusters::ServerCluster;
use crate::schema::server_clusters;
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpResponse, web};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use diesel::prelude::*;
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::{
    Client, Config,
    api::{Api, ListParams},
};
use serde_json::json;
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

pub async fn handle_get_first_server_cluster(context: web::Data<AppContext>) -> HttpResponse {
    let mut conn = context.pool.get().expect("DB conn");

    let server_cluster = match server_clusters::table
        .select(ServerCluster::as_select())
        .first::<ServerCluster>(&mut conn)
        .optional()
        .expect("DB query failed")
    {
        Some(cluster) => cluster,
        None => {
            return HttpResponse::NotFound().json(api_error("No server cluster found", json!({})));
        }
    };

    let k8s_client =
        match K8sClient::from_server_cluster(&server_cluster, Some("default".to_string())).await {
            Ok(client) => client,
            Err(e) => {
                return HttpResponse::InternalServerError().json(api_error(
                    &format!("Failed to create K8s client: {}", e),
                    json!({}),
                ));
            }
        };

    let pods: Api<Pod> = Api::namespaced(k8s_client.client, &k8s_client.namespace);

    match pods.list(&ListParams::default()).await {
        Ok(pod_list) => {
            let pod_names: Vec<String> = pod_list
                .items
                .iter()
                .filter_map(|pod| pod.metadata.name.clone())
                .collect();

            let pods_info: Vec<serde_json::Value> = pod_list
                .items
                .iter()
                .map(|pod| {
                    json!({
                        "name": pod.metadata.name,
                        "namespace": pod.metadata.namespace,
                        "phase": pod.status.as_ref().and_then(|s| s.phase.clone()),
                        "labels": pod.metadata.labels,
                        "creation_timestamp": pod.metadata.creation_timestamp,
                    })
                })
                .collect();

            HttpResponse::Ok().json(api_success(
                &format!("Found {} pods", pod_names.len()),
                json!({
                    "pods": pods_info,
                    "total": pod_names.len(),
                    "cluster": server_cluster.environment,
                }),
            ))
        }
        Err(e) => HttpResponse::InternalServerError().json(api_error(
            &format!("Failed to fetch pods: {}", e),
            json!({}),
        )),
    }
}

pub async fn handle_get_first_server_cluster_nodes(context: web::Data<AppContext>) -> HttpResponse {
    let mut conn = context.pool.get().expect("DB conn");

    let server_cluster = match server_clusters::table
        .select(ServerCluster::as_select())
        .first::<ServerCluster>(&mut conn)
        .optional()
        .expect("DB query failed")
    {
        Some(cluster) => cluster,
        None => {
            return HttpResponse::NotFound().json(api_error("No server cluster found", json!({})));
        }
    };

    let k8s_client =
        match K8sClient::from_server_cluster(&server_cluster, Some("default".to_string())).await {
            Ok(client) => client,
            Err(e) => {
                return HttpResponse::InternalServerError().json(api_error(
                    &format!("Failed to create K8s client: {}", e),
                    json!({}),
                ));
            }
        };

    let nodes: Api<Node> = Api::all(k8s_client.client);

    match nodes.list(&ListParams::default()).await {
        Ok(node_list) => {
            let node_names: Vec<String> = node_list
                .items
                .iter()
                .filter_map(|node| node.metadata.name.clone())
                .collect();

            let nodes_info: Vec<serde_json::Value> = node_list
                .items
                .iter()
                .map(|node| {
                    json!({
                        "name": node.metadata.name,
                        "labels": node.metadata.labels,
                        "creation_timestamp": node.metadata.creation_timestamp,
                        "conditions": node.status.as_ref().and_then(|s| s.conditions.as_ref()).map(|conditions| {
                            conditions.iter().map(|cond| {
                                json!({
                                    "type": cond.type_,
                                    "status": cond.status,
                                    "reason": cond.reason,
                                    "message": cond.message,
                                    "last_heartbeat_time": cond.last_heartbeat_time,
                                    "last_transition_time": cond.last_transition_time,
                                })
                            }).collect::<Vec<_>>()
                        }),
                        "capacity": node.status.as_ref().and_then(|s| s.capacity.as_ref()).map(|capacity| {
                            let mut cap_map = serde_json::Map::new();
                            for (key, value) in capacity {
                                cap_map.insert(key.clone(), json!(value.0));
                            }
                            json!(cap_map)
                        }),
                        "allocatable": node.status.as_ref().and_then(|s| s.allocatable.as_ref()).map(|allocatable| {
                            let mut alloc_map = serde_json::Map::new();
                            for (key, value) in allocatable {
                                alloc_map.insert(key.clone(), json!(value.0));
                            }
                            json!(alloc_map)
                        }),
                        "node_info": node.status.as_ref().and_then(|s| s.node_info.as_ref()).map(|info| {
                            json!({
                                "architecture": info.architecture,
                                "boot_id": info.boot_id,
                                "container_runtime_version": info.container_runtime_version,
                                "kernel_version": info.kernel_version,
                                "kube_proxy_version": info.kube_proxy_version,
                                "kubelet_version": info.kubelet_version,
                                "machine_id": info.machine_id,
                                "operating_system": info.operating_system,
                                "os_image": info.os_image,
                                "system_uuid": info.system_uuid,
                            })
                        }),
                        "addresses": node.status.as_ref().and_then(|s| s.addresses.as_ref()).map(|addresses| {
                            addresses.iter().map(|addr| {
                                json!({
                                    "type": addr.type_,
                                    "address": addr.address,
                                })
                            }).collect::<Vec<_>>()
                        }),
                    })
                })
                .collect();

            HttpResponse::Ok().json(api_success(
                &format!("Found {} nodes", node_names.len()),
                json!({
                    "nodes": nodes_info,
                    "total": node_names.len(),
                    "cluster": server_cluster.environment,
                }),
            ))
        }
        Err(e) => HttpResponse::InternalServerError().json(api_error(
            &format!("Failed to fetch nodes: {}", e),
            json!({}),
        )),
    }
}

pub async fn handle_deploy_hello_world_pod(context: web::Data<AppContext>) -> HttpResponse {
    use k8s_openapi::api::core::v1::{Container, Pod, PodSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    let mut conn = context.pool.get().expect("DB conn");

    let server_cluster = match server_clusters::table
        .select(ServerCluster::as_select())
        .first::<ServerCluster>(&mut conn)
        .optional()
        .expect("DB query failed")
    {
        Some(cluster) => cluster,
        None => {
            return HttpResponse::NotFound().json(api_error("No server cluster found", json!({})));
        }
    };

    let k8s_client =
        match K8sClient::from_server_cluster(&server_cluster, Some("default".to_string())).await {
            Ok(client) => client,
            Err(e) => {
                return HttpResponse::InternalServerError().json(api_error(
                    &format!("Failed to create K8s client: {}", e),
                    json!({}),
                ));
            }
        };

    let pods: Api<Pod> = Api::namespaced(k8s_client.client, &k8s_client.namespace);

    let hello_pod = Pod {
        metadata: ObjectMeta {
            name: Some("hello-world-pod".to_string()),
            namespace: Some(k8s_client.namespace.clone()),
            ..Default::default()
        },
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "hello-world".to_string(),
                image: Some("hello-world:latest".to_string()),
                ..Default::default()
            }],
            restart_policy: Some("Never".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    match pods.create(&Default::default(), &hello_pod).await {
        Ok(pod) => HttpResponse::Ok().json(api_success(
            "Pod created successfully",
            json!({"name": pod.metadata.name}),
        )),
        Err(e) => HttpResponse::InternalServerError().json(api_error(
            &format!("Failed to deploy pod: {}", e),
            json!({}),
        )),
    }
}
