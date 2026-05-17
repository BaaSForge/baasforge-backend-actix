use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
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
}

impl fmt::Display for RuntimeNatsPublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeNatsPublishError::Serialize(e) => write!(f, "serialize NATS payload: {}", e),
            RuntimeNatsPublishError::Publish(s) => write!(f, "nats publish: {}", s),
        }
    }
}

impl std::error::Error for RuntimeNatsPublishError {}

/// JetStream context on a shared [`async_nats::Client`]. Release events must use JetStream publish
/// so messages land in the stream (core publish does not unless the server mirrors it).
pub struct RuntimeNatsPublisher {
    jetstream: async_nats::jetstream::Context,
}

impl RuntimeNatsPublisher {
    pub async fn connect(url: &str) -> Result<Self, async_nats::Error> {
        // `async_nats::connect` does not apply user:pass from the URL to the CONNECT frame; only
        // `ConnectOptions` auth is sent. Use explicit options when the URL embeds credentials.
        let server_addr: async_nats::ServerAddr = url.parse()?;
        let mut options = async_nats::ConnectOptions::new();
        if server_addr.has_user_pass() {
            let user = server_addr.username().map(String::from).unwrap_or_default();
            let pass = server_addr.password().map(String::from).unwrap_or_default();
            options = async_nats::ConnectOptions::with_user_and_password(user, pass);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime NATS: missing user and password in URL (use nats://user:pass@host:port)",
            )
            .into());
        }
        let client = async_nats::connect_with_options(server_addr, options).await?;
        let jetstream = async_nats::jetstream::new(client);
        Ok(Self { jetstream })
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
