use crate::middlewares::auth::AccessPayload;
use crate::routes::AppContext;
use crate::routes::ws::broker::{ConnectionId, WsBroker};
use crate::routes::ws::events::WsServerEvent;
use crate::routes::ws::topics;
use actix_web::web;
use actix_web::{HttpMessage, HttpRequest};
use actix_ws::Message;
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::StreamExt as _;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe { topic: String },
    Unsubscribe { topic: String },
}

pub async fn handle_ws(
    context: web::Data<AppContext>,
    req: HttpRequest,
    body: web::Payload,
) -> actix_web::Result<impl actix_web::Responder> {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist (auth_closed_guard_middleware)");
    let user_id = access_payload.user_id;

    let broker = context.ws_broker.clone();

    log::info!(
        "[ws] WebSocket handshake start: user_id={}, uri={}, headers={:?}",
        user_id,
        req.uri(),
        req.headers()
    );

    let (response, mut session, mut msg_stream) = match actix_ws::handle(&req, body) {
        Ok(ok) => ok,
        Err(e) => {
            log::error!(
                "[ws] Error during WebSocket handshake: {}; user_id={}, uri={}, headers={:?}",
                e,
                user_id,
                req.uri(),
                req.headers()
            );
            return Err(actix_web::error::ErrorInternalServerError(e));
        }
    };

    let pool = context.pool.clone();
    let broker = broker.clone();
    actix_web::rt::spawn(async move {
        let (conn_id, mut rx) = broker.register().await;
        let _guard = WsConnectionGuard {
            broker: broker.clone(),
            conn_id,
        };

        log::info!(
            "[ws] Spawned connection handler for user_id={} conn_id={}",
            user_id,
            conn_id
        );

        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    log::debug!("[ws] Sending msg to client via WebSocket: conn_id={} msg={}", conn_id, msg);
                    if let Err(e) = session.text(msg).await {
                        log::warn!("[ws] Failed to send message to WebSocket client: conn_id={} error={}", conn_id, e);
                        break;
                    }
                }
                next = msg_stream.next() => {
                    match next {
                        Some(Ok(Message::Ping(bytes))) => {
                            log::debug!("[ws] Received Ping from client: conn_id={} bytes={:?}", conn_id, bytes);
                            if let Err(e) = session.pong(&bytes).await {
                                log::warn!("[ws] Failed to send Pong: conn_id={} error={}", conn_id, e);
                                break;
                            }
                        }
                        Some(Ok(Message::Text(text))) => {
                            log::debug!("[ws] Received Text from client: conn_id={} text={}", conn_id, text);
                            if let Err(e) = handle_client_message(
                                &broker,
                                conn_id,
                                user_id,
                                &pool,
                                &text,
                                &mut session,
                            ).await {
                                log::warn!("[ws] handle_client_message: {}", e);
                            }
                        }
                        Some(Ok(Message::Close(reason))) => {
                            log::info!("[ws] Received Close from client: conn_id={} reason={:?}", conn_id, reason);
                            break;
                        }
                        None => {
                            log::info!("[ws] Client disconnected: conn_id={}", conn_id);
                            break;
                        }
                        Some(Ok(other)) => {
                            log::debug!("[ws] Received other frame: conn_id={} {:?}", conn_id, other);
                        }
                        Some(Err(e)) => {
                            log::error!("[ws] Error receiving from client: conn_id={} error={}", conn_id, e);
                            break;
                        }
                    }
                }
            }
        }
        log::info!("[ws] WebSocket connection closing for conn_id={}", conn_id);
        if let Err(e) = session.close(None).await {
            log::warn!(
                "[ws] Error closing WebSocket session: conn_id={} error={}",
                conn_id,
                e
            );
        }
    });

    log::info!(
        "[ws] WebSocket handshake successful, returning response for user_id={}",
        user_id
    );
    Ok(response)
}

struct WsConnectionGuard {
    broker: Arc<WsBroker>,
    conn_id: ConnectionId,
}

impl Drop for WsConnectionGuard {
    fn drop(&mut self) {
        let broker = self.broker.clone();
        let conn_id = self.conn_id;
        actix_web::rt::spawn(async move {
            broker.unregister(conn_id).await;
        });
    }
}

async fn handle_client_message(
    broker: &WsBroker,
    conn_id: ConnectionId,
    user_id: uuid::Uuid,
    pool: &crate::database::DbPool,
    text: &str,
    session: &mut actix_ws::Session,
) -> Result<(), String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {}", e))?;
    match msg {
        ClientMessage::Subscribe { topic } => {
            let auth = topics::authorize_topic_async(&topic, user_id, pool).await;
            match auth {
                topics::TopicAuth::Allowed(canonical_topic) => {
                    broker.subscribe(conn_id, canonical_topic.clone()).await;
                    let event = WsServerEvent::Subscribed {
                        topic: canonical_topic,
                    };
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    session.text(out).await.map_err(|e| e.to_string())?;
                }
                topics::TopicAuth::Denied(reason) => {
                    let event = WsServerEvent::Error {
                        message: reason.to_string(),
                    };
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    session.text(out).await.map_err(|e| e.to_string())?;
                }
            }
        }
        ClientMessage::Unsubscribe { topic } => {
            // Only allow unsubscribing from topics this user is allowed to subscribe to (defense in depth)
            let auth = topics::authorize_topic_async(&topic, user_id, pool).await;
            match auth {
                topics::TopicAuth::Allowed(canonical_topic) => {
                    broker.unsubscribe(conn_id, &canonical_topic).await;
                    let event = WsServerEvent::Unsubscribed {
                        topic: canonical_topic,
                    };
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    session.text(out).await.map_err(|e| e.to_string())?;
                }
                topics::TopicAuth::Denied(_) => {
                    // Ignore unsub for topics user cannot access; don't leak that the topic exists
                    let event = WsServerEvent::Unsubscribed { topic };
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    session.text(out).await.map_err(|e| e.to_string())?;
                }
            }
        }
    }
    Ok(())
}
