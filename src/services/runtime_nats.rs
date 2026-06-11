use serde::{Deserialize, Serialize};
use std::cmp;
use std::fmt;
use std::io;
use std::time::Duration;
use uuid::Uuid;

/// Matches the consumer’s [`ReleaseDeployEnvironment`] (JSON: `production` / `stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseDeployEnvironment {
    Production,
    Stage,
}

/// Payload published when a runtime’s live release changes (upgrade, rollback, first deploy, or detach).
///
/// **Identity:** Consumers must resolve the release using `deployed_release_id` / `previous_release_id`
/// (both are `project_releases.id` UUIDs). Release **revision** (`project_releases.revision`, a float)
/// is **not** included and must **not** be used to pick a row: the same revision can exist on multiple
/// rows (e.g. different chat sessions).
#[derive(Debug, Serialize, Deserialize)]
pub struct ReleaseDeployEvent {
    pub project_id: Uuid,
    pub environment: ReleaseDeployEnvironment,
    #[serde(default)]
    pub runtime_id: Option<Uuid>,
    /// `project_releases.id` for the release now live (`null` when detached / no deploy).
    #[serde(default)]
    pub deployed_release_id: Option<Uuid>,
    /// `project_releases.id` that was live immediately before this event. `null` when there was
    /// none (e.g. first release deployed to this runtime / environment).
    #[serde(default)]
    pub previous_release_id: Option<Uuid>,
}

#[derive(Debug)]
pub enum RuntimeNatsPublishError {
    Serialize(serde_json::Error),
    Publish(String),
    TimedOut,
    Unavailable,
}

impl fmt::Display for RuntimeNatsPublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeNatsPublishError::Serialize(e) => write!(f, "serialize NATS payload: {}", e),
            RuntimeNatsPublishError::Publish(s) => write!(f, "nats publish: {}", s),
            RuntimeNatsPublishError::TimedOut => write!(f, "nats operation timed out"),
            RuntimeNatsPublishError::Unavailable => {
                write!(f, "runtime NATS publisher is not configured")
            }
        }
    }
}

impl std::error::Error for RuntimeNatsPublishError {}

fn runtime_nats_connect_options(user: String, pass: String) -> async_nats::ConnectOptions {
    async_nats::ConnectOptions::with_user_and_password(user, pass)
        .name("baal-backend-runtime-nats")
        .retry_on_initial_connect()
        .max_reconnects(None)
        .connection_timeout(Duration::from_secs(5))
        .reconnect_delay_callback(|attempts| {
            let secs = cmp::min(cmp::max(attempts as u64, 1), 30);
            Duration::from_secs(secs)
        })
}

fn parse_runtime_nats_url(url: &str) -> Result<(async_nats::ServerAddr, String, String), io::Error> {
    let server_addr: async_nats::ServerAddr = url
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if !server_addr.has_user_pass() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime NATS: missing user and password in URL (use nats://user:pass@host:port)",
        ));
    }
    let user = server_addr.username().map(String::from).unwrap_or_default();
    let pass = server_addr.password().map(String::from).unwrap_or_default();
    Ok((server_addr, user, pass))
}

/// JetStream context on a shared [`async_nats::Client`]. Release events must use JetStream publish
/// so messages land in the stream (core publish does not unless the server mirrors it).
///
/// The underlying [`async_nats::Client`] reconnects indefinitely via crate defaults; API publishes
/// are bounded by [`Self::operation_timeout`] with one retry (no in-memory buffering).
pub struct RuntimeNatsPublisher {
    jetstream: async_nats::jetstream::Context,
    operation_timeout: Duration,
}

impl RuntimeNatsPublisher {
    pub async fn connect(
        url: &str,
        operation_timeout: Duration,
    ) -> Result<Self, async_nats::Error> {
        let (server_addr, user, pass) = parse_runtime_nats_url(url)?;
        let options = runtime_nats_connect_options(user, pass);
        let client = options.connect(server_addr).await?;
        let jetstream = async_nats::jetstream::new(client);
        Ok(Self {
            jetstream,
            operation_timeout,
        })
    }

    pub async fn ping(&self) -> Result<(), RuntimeNatsPublishError> {
        use async_nats::connection::State;
        let client = self.jetstream.client();
        if client.connection_state() != State::Connected {
            return Err(RuntimeNatsPublishError::Unavailable);
        }
        match tokio::time::timeout(self.operation_timeout, client.flush()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(RuntimeNatsPublishError::Publish(format!("nats ping: {e}"))),
            Err(_) => Err(RuntimeNatsPublishError::TimedOut),
        }
    }

    async fn publish_bytes_once(
        &self,
        subject: &str,
        bytes: Vec<u8>,
        event_kind: &'static str,
    ) -> Result<(), RuntimeNatsPublishError> {
        self.jetstream
            .publish(subject.to_string(), bytes.into())
            .await
            .map_err(|e| RuntimeNatsPublishError::Publish(e.to_string()))?;
        tracing::info!(
            target: "runtime_nats",
            subject = %subject,
            event_kind,
            "runtime event published via JetStream successfully"
        );
        Ok(())
    }

    async fn publish_bytes_with_recovery(
        &self,
        subject: &str,
        bytes: Vec<u8>,
        event_kind: &'static str,
    ) -> Result<(), RuntimeNatsPublishError> {
        let err = match tokio::time::timeout(
            self.operation_timeout,
            self.publish_bytes_once(subject, bytes.clone(), event_kind),
        )
        .await
        {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => e,
            Err(_) => RuntimeNatsPublishError::TimedOut,
        };
        tracing::warn!(
            target: "runtime_nats",
            subject = %subject,
            event_kind,
            error = %err,
            "runtime NATS publish failed; retrying once"
        );
        match tokio::time::timeout(
            self.operation_timeout,
            self.publish_bytes_once(subject, bytes, event_kind),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(RuntimeNatsPublishError::TimedOut),
        }
    }

    /// Publish arbitrary JSON to a JetStream subject (core publish does not land in the stream).
    async fn publish_json<T: Serialize>(
        &self,
        subject: &str,
        payload: &T,
        event_kind: &'static str,
    ) -> Result<(), RuntimeNatsPublishError> {
        let bytes = serde_json::to_vec(payload).map_err(RuntimeNatsPublishError::Serialize)?;
        let payload_json = String::from_utf8_lossy(&bytes);
        tracing::info!(
            target: "runtime_nats",
            subject = %subject,
            payload = %payload_json,
            event_kind,
            "publishing runtime event via JetStream (for runtimes to consume)"
        );
        self.publish_bytes_with_recovery(subject, bytes, event_kind)
            .await
    }

    pub async fn publish_stage_release(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
        deployed_release_id: Uuid,
        previous_release_id: Option<Uuid>,
    ) -> Result<(), RuntimeNatsPublishError> {
        self.publish_stage_runtime_release_state(
            subject,
            project_id,
            runtime_id,
            Some(deployed_release_id),
            previous_release_id,
        )
        .await
    }

    /// Stage runtime no longer has a live release (`deployed_release_id` null). Same subject as deploy events.
    pub async fn publish_stage_release_cleared(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
        previous_release_id: Option<Uuid>,
    ) -> Result<(), RuntimeNatsPublishError> {
        self.publish_stage_runtime_release_state(
            subject,
            project_id,
            runtime_id,
            None,
            previous_release_id,
        )
        .await
    }

    /// `Some(deployed_release_id)` = stage deploy; `None` = detached / no live release.
    pub async fn publish_stage_runtime_release_state(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
        deployed_release_id: Option<Uuid>,
        previous_release_id: Option<Uuid>,
    ) -> Result<(), RuntimeNatsPublishError> {
        match deployed_release_id {
            Some(dr) => {
                let payload = ReleaseDeployEvent {
                    project_id,
                    environment: ReleaseDeployEnvironment::Stage,
                    runtime_id: Some(runtime_id),
                    deployed_release_id: Some(dr),
                    previous_release_id,
                };
                self.publish_json(subject, &payload, "release").await
            }
            None => {
                tracing::info!(
                    target: "runtime_nats",
                    subject = %subject,
                    project_id = %project_id,
                    runtime_id = %runtime_id,
                    "publishing stage live release CLEARED (detach): no deployed_release_id for runtime consumer"
                );
                let payload = ReleaseDeployEvent {
                    project_id,
                    environment: ReleaseDeployEnvironment::Stage,
                    runtime_id: Some(runtime_id),
                    deployed_release_id: None,
                    previous_release_id,
                };
                self.publish_json(subject, &payload, "release").await
            }
        }
    }

    pub async fn publish_prod_release(
        &self,
        subject: &str,
        project_id: Uuid,
        deployed_release_id: Uuid,
        previous_release_id: Option<Uuid>,
    ) -> Result<(), RuntimeNatsPublishError> {
        let payload = ReleaseDeployEvent {
            project_id,
            environment: ReleaseDeployEnvironment::Production,
            runtime_id: None,
            deployed_release_id: Some(deployed_release_id),
            previous_release_id,
        };
        self.publish_json(subject, &payload, "release").await
    }
}

/// Payload published when a project runtime env var is created, updated, or deleted.
#[derive(Debug, Serialize, Deserialize)]
pub struct RuntimeEnvInvalidateEvent {
    pub project_id: Uuid,
    pub runtime_id: Uuid,
    pub key: String,
}

impl RuntimeNatsPublisher {
    pub async fn publish_runtime_env_invalidate(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
        key: &str,
    ) -> Result<(), RuntimeNatsPublishError> {
        let payload = RuntimeEnvInvalidateEvent {
            project_id,
            runtime_id,
            key: key.to_string(),
        };
        self.publish_json(subject, &payload, "env").await
    }
}
