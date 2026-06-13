use std::sync::OnceLock;
use std::time::Duration;
use crate::configs::Settings;
use crate::routes::AppContext;
use actix_web::http::header::{self};
use actix_web::{HttpRequest, HttpResponse, web};
use actix_ws::Message as ClientWsMessage;
use futures::{SinkExt, StreamExt as FuturesStreamExt};
use reqwest::header::HeaderValue as ReqwestHeaderValue;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName as HttpHeaderName, HeaderValue as HttpHeaderValue};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{self, Message as UpstreamWsMessage};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const X_PRIVATE_API_KEY: &str = "x-private-api-key";
const MAX_PROXY_BODY_BYTES: usize = 512 * 1024 * 1024;

fn skip_forwarded_request_header(name: &str) -> bool {
    is_hop_by_hop_request_header(name)
        || name.eq_ignore_ascii_case(X_PRIVATE_API_KEY)
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("transfer-encoding")
}

fn runtime_proxy_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("private_proxy reqwest client")
    })
}

fn normalize_localhost(endpoint: &str) -> String {
    endpoint.replace("://localhost", "://127.0.0.1")
}

fn is_hop_by_hop_request_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

fn is_hop_by_hop_response_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn skip_ws_upstream_request_header(name: &str) -> bool {
    is_hop_by_hop_request_header(name)
        || name.eq_ignore_ascii_case(X_PRIVATE_API_KEY)
        || name.eq_ignore_ascii_case("sec-websocket-key")
        || name.eq_ignore_ascii_case("sec-websocket-version")
        || name.eq_ignore_ascii_case("sec-websocket-extensions")
}

fn is_websocket_upgrade(req: &HttpRequest) -> bool {
    let upgrade = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.eq_ignore_ascii_case("websocket"));
    let connection = req
        .headers()
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.to_ascii_lowercase().contains("upgrade"));
    upgrade && connection
}

fn build_target_http_url(settings: &Settings, tail: &str, query: &str) -> String {
    let base = normalize_localhost(settings.base_runtime_endpoint.trim_end_matches('/'));
    let mut url = format!("{base}/{}", tail.trim_start_matches('/'));
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    url
}

fn http_url_to_ws_url(http_url: &str) -> String {
    if let Some(rest) = http_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("wss://{http_url}")
    }
}

fn private_api_key_header_value(api_key: &str) -> Result<ReqwestHeaderValue, HttpResponse> {
    ReqwestHeaderValue::from_str(api_key).map_err(|e| {
        log::error!("[private_proxy] invalid RUNTIME_PRIVATE_API_KEY: {e}");
        HttpResponse::InternalServerError().finish()
    })
}

pub async fn handle_private_proxy(
    context: web::Data<AppContext>,
    req: HttpRequest,
    payload: web::Payload,
    path: web::Path<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let tail = path.into_inner();
    let query = req.query_string();
    let target_url = build_target_http_url(&context.settings, &tail, query);

    if is_websocket_upgrade(&req) {
        return proxy_websocket(&context, req, payload, target_url).await;
    }

    Ok(proxy_http(&context, req, payload, target_url).await)
}

async fn read_payload(mut payload: web::Payload) -> Result<Vec<u8>, HttpResponse> {
    let mut body = web::BytesMut::new();
    while let Some(chunk) = payload.next().await {
        let chunk = chunk.map_err(|e| {
            log::error!("[private_proxy] failed to read request body: {e}");
            HttpResponse::BadRequest().finish()
        })?;
        if body.len() + chunk.len() > MAX_PROXY_BODY_BYTES {
            return Err(HttpResponse::PayloadTooLarge().finish());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.to_vec())
}

async fn proxy_http(
    context: &AppContext,
    req: HttpRequest,
    payload: web::Payload,
    target_url: String,
) -> HttpResponse {
    let client = runtime_proxy_http_client();

    let method = match reqwest::Method::from_bytes(req.method().as_str().as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            log::error!("[private_proxy] unsupported HTTP method: {e}");
            return HttpResponse::BadRequest().finish();
        }
    };
    let mut forward = client.request(method, &target_url);

    for (name, value) in req.headers().iter() {
        if skip_forwarded_request_header(name.as_str()) {
            continue;
        }
        forward = forward.header(name.as_str(), value.as_bytes());
    }

    let api_key = match private_api_key_header_value(&context.settings.runtime_private_api_key) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    forward = forward.header(X_PRIVATE_API_KEY, api_key);

    let body = match read_payload(payload).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if !body.is_empty() {
        forward = forward.body(body);
    }

    let upstream = match forward.send().await {
        Ok(r) => r,
        Err(e) => {
            log::error!(
                "[private_proxy] upstream request failed url={target_url}: {e:#}"
            );
            return HttpResponse::BadGateway().finish();
        }
    };

    let status = actix_web::http::StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY);
    let mut response = HttpResponse::build(status);

    for (name, value) in upstream.headers().iter() {
        if is_hop_by_hop_response_header(name.as_str()) {
            continue;
        }
        response.insert_header((name.as_str(), value.as_bytes()));
    }

    response.streaming(upstream.bytes_stream().map(|chunk| {
        chunk.map_err(actix_web::error::ErrorBadGateway)
    }))
}

async fn proxy_websocket(
    context: &AppContext,
    req: HttpRequest,
    payload: web::Payload,
    target_http_url: String,
) -> Result<HttpResponse, actix_web::Error> {
    let forwarded_headers: Vec<(String, Vec<u8>)> = req
        .headers()
        .iter()
        .filter(|(name, _)| !skip_ws_upstream_request_header(name.as_str()))
        .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
        .collect();

    let (response, session, client_stream) = actix_ws::handle(&req, payload)?;

    let ws_url = http_url_to_ws_url(&target_http_url);
    let api_key = context.settings.runtime_private_api_key.clone();

    actix_web::rt::spawn(async move {
        let upstream =
            match connect_upstream_websocket(&forwarded_headers, &ws_url, &api_key).await {
                Ok(stream) => stream,
                Err(e) => {
                    log::error!(
                        "[private_proxy] upstream websocket connect failed url={ws_url}: {e}"
                    );
                    let _ = session.close(None).await;
                    return;
                }
            };

        relay_websocket(session, client_stream, upstream).await;
    });

    Ok(response)
}

async fn connect_upstream_websocket(
    forwarded_headers: &[(String, Vec<u8>)],
    ws_url: &str,
    api_key: &str,
) -> Result<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, tungstenite::Error> {
    let mut request = ws_url
        .into_client_request()
        .map_err(tungstenite::Error::from)?;

    for (name, value) in forwarded_headers {
        let name = HttpHeaderName::from_bytes(name.as_bytes()).map_err(tungstenite::Error::from)?;
        let value = HttpHeaderValue::from_bytes(value).map_err(tungstenite::Error::from)?;
        request.headers_mut().append(name, value);
    }

    let api_key_value = HttpHeaderValue::from_str(api_key).map_err(tungstenite::Error::from)?;
    request.headers_mut().insert(
        HttpHeaderName::from_static(X_PRIVATE_API_KEY),
        api_key_value,
    );

    let (stream, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(stream)
}

async fn relay_websocket(
    mut session: actix_ws::Session,
    mut client_stream: actix_ws::MessageStream,
    upstream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) {
    let (mut upstream_sink, mut upstream_stream) = upstream.split();

    loop {
        tokio::select! {
            client_msg = client_stream.next() => {
                match client_msg {
                    Some(Ok(ClientWsMessage::Text(text))) => {
                        if upstream_sink
                            .send(UpstreamWsMessage::Text(text.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(ClientWsMessage::Binary(bin))) => {
                        if upstream_sink.send(UpstreamWsMessage::Binary(bin)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientWsMessage::Ping(bytes))) => {
                        if upstream_sink.send(UpstreamWsMessage::Ping(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientWsMessage::Pong(bytes))) => {
                        if upstream_sink.send(UpstreamWsMessage::Pong(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientWsMessage::Close(reason))) => {
                        let upstream_close = reason
                            .as_ref()
                            .map(|r| {
                                UpstreamWsMessage::Close(Some(CloseFrame {
                                    code: tungstenite::protocol::frame::coding::CloseCode::from(
                                        u16::from(r.code),
                                    ),
                                    reason: r.description.clone().unwrap_or_default().into(),
                                }))
                            })
                            .unwrap_or(UpstreamWsMessage::Close(None));
                        let _ = upstream_sink.send(upstream_close).await;
                        let _ = session.close(reason).await;
                        break;
                    }
                    Some(Ok(ClientWsMessage::Continuation(_))) | Some(Ok(ClientWsMessage::Nop)) => {}
                    Some(Err(e)) => {
                        log::error!("[private_proxy] client websocket error: {e}");
                        break;
                    }
                    None => break,
                }
            }
            upstream_msg = upstream_stream.next() => {
                match upstream_msg {
                    Some(Ok(UpstreamWsMessage::Text(text))) => {
                        if session.text(text.to_string()).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpstreamWsMessage::Binary(bin))) => {
                        if session.binary(bin).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpstreamWsMessage::Ping(bytes))) => {
                        if session.pong(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpstreamWsMessage::Pong(bytes))) => {
                        if session.pong(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpstreamWsMessage::Close(reason))) => {
                        let actix_reason = reason.map(|frame| actix_ws::CloseReason {
                            code: actix_ws::CloseCode::from(u16::from(frame.code)),
                            description: Some(frame.reason.to_string()),
                        });
                        let _ = session.close(actix_reason).await;
                        break;
                    }
                    Some(Ok(UpstreamWsMessage::Frame(_))) => {}
                    Some(Err(e)) => {
                        log::error!("[private_proxy] upstream websocket error: {e}");
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}
