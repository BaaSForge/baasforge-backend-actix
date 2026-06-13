use async_nats::jetstream;
use async_nats::jetstream::stream;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEntry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub kind: String,
    pub metric: String,
    pub quantity: i64,
    pub price_unit: String,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct UsagePublisherConfig {
    pub runtime_nats_url: String,
    pub stream_name: String,
    pub subject: String,
    pub batch_size: usize,
    pub flush_ms: u64,
    pub channel_capacity: usize,
}

static USAGE_TX: OnceLock<mpsc::Sender<UsageEntry>> = OnceLock::new();

pub fn enqueue(entry: UsageEntry) {
    if let Some(tx) = USAGE_TX.get() {
        let _ = tx.try_send(entry);
    }
}

pub fn start_usage_publisher(config: UsagePublisherConfig) {
    let (tx, rx) = mpsc::channel(config.channel_capacity);
    let _ = USAGE_TX.set(tx);
    tokio::spawn(run_publisher(config, rx));
}

async fn run_publisher(config: UsagePublisherConfig, mut rx: mpsc::Receiver<UsageEntry>) {
    let client = match crate::services::runtime_nats::connect_nats(&config.runtime_nats_url).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "[usage_publisher] NATS connect failed");
            return;
        }
    };
    tracing::info!("[usage_publisher] NATS client ready");

    let js = jetstream::new(client);
    if let Err(e) = js
        .get_or_create_stream(stream::Config {
            name: config.stream_name.clone(),
            subjects: vec![config.subject.clone()],
            ..Default::default()
        })
        .await
    {
        tracing::error!(error = %e, "[usage_publisher] stream setup failed");
        return;
    }

    let mut buffer = Vec::with_capacity(config.batch_size);
    let mut interval = tokio::time::interval(Duration::from_millis(config.flush_ms));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            entry = rx.recv() => {
                match entry {
                    Some(e) => {
                        buffer.push(e);
                        if buffer.len() >= config.batch_size {
                            spawn_flush(&js, &config.subject, &mut buffer);
                        }
                    }
                    None => break,
                }
            }
            _ = interval.tick() => {
                if !buffer.is_empty() {
                    spawn_flush(&js, &config.subject, &mut buffer);
                }
            }
        }
    }
}

fn spawn_flush(js: &jetstream::Context, subject: &str, buffer: &mut Vec<UsageEntry>) {
    let batch = std::mem::take(buffer);
    let count = batch.len();
    let payload = match serde_json::to_vec(&batch) {
        Ok(bytes) => Bytes::from(bytes),
        Err(e) => {
            tracing::warn!(error = %e, count, "[usage_publisher] JSON encode failed; dropped batch");
            return;
        }
    };
    let js = js.clone();
    let subject = subject.to_string();
    tokio::spawn(async move {
        if let Err(e) = crate::services::runtime_nats::publish_jetstream(&js, subject, payload).await {
            tracing::warn!(error = %e, count, "[usage_publisher] publish failed; dropped batch");
        }
    });
}
