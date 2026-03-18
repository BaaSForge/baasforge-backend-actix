use crate::routes::ws::events::WsServerEvent;
use serde_json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{RwLock, mpsc};

pub type ConnectionId = u64;

struct WsSubscriber {
    tx: mpsc::UnboundedSender<String>,
    topics: HashSet<String>,
}

#[derive(Clone)]
pub struct WsBroker {
    next_id: std::sync::Arc<AtomicU64>,
    inner: std::sync::Arc<RwLock<HashMap<ConnectionId, WsSubscriber>>>,
}

impl Default for WsBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl WsBroker {
    pub fn new() -> Self {
        Self {
            next_id: std::sync::Arc::new(AtomicU64::new(0)),
            inner: std::sync::Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(&self) -> (ConnectionId, mpsc::UnboundedReceiver<String>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        let mut guard = self.inner.write().await;
        guard.insert(
            id,
            WsSubscriber {
                tx,
                topics: HashSet::new(),
            },
        );
        (id, rx)
    }

    pub async fn unregister(&self, id: ConnectionId) {
        let mut guard = self.inner.write().await;
        guard.remove(&id);
    }

    pub async fn subscribe(&self, id: ConnectionId, topic: String) -> bool {
        let mut guard = self.inner.write().await;
        if let Some(sub) = guard.get_mut(&id) {
            sub.topics.insert(topic);
            true
        } else {
            false
        }
    }

    pub async fn unsubscribe(&self, id: ConnectionId, topic: &str) -> bool {
        let mut guard = self.inner.write().await;
        if let Some(sub) = guard.get_mut(&id) {
            sub.topics.remove(topic);
            true
        } else {
            false
        }
    }

    pub async fn broadcast_to_topic(&self, topic: &str, message: &str) {
        let senders: Vec<mpsc::UnboundedSender<String>> = {
            let guard = self.inner.read().await;
            guard
                .values()
                .filter(|sub| sub.topics.contains(topic))
                .map(|sub| sub.tx.clone())
                .collect()
        };
        for tx in senders {
            let _ = tx.send(message.to_string());
        }
    }

    pub async fn broadcast_event_to_topic(&self, topic: &str, event: &WsServerEvent) {
        match serde_json::to_string(event) {
            Ok(msg) => self.broadcast_to_topic(topic, &msg).await,
            Err(e) => log::warn!("[ws] Failed to serialize event: {}", e),
        }
    }

    pub async fn chat_session_topics_with_subscribers(&self) -> Vec<String> {
        let guard = self.inner.read().await;
        let mut topics = std::collections::HashSet::new();
        for sub in guard.values() {
            for t in &sub.topics {
                if t.starts_with("chat_session:") {
                    topics.insert(t.clone());
                }
            }
        }
        topics.into_iter().collect()
    }
}
