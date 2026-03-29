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
#[derive(Debug, Serialize, Deserialize)]
pub struct ReleaseDeployEvent {
    pub project_id: Uuid,
    pub environment: ReleaseDeployEnvironment,
    #[serde(default)]
    pub runtime_id: Option<Uuid>,
    /// `None` / JSON `null`: no live release (e.g. stage runtime detached). Otherwise the deployed revision.
    pub deployed_revision: Option<i32>,
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
            let user = server_addr
                .username()
                .map(String::from)
                .unwrap_or_default();
            let pass = server_addr
                .password()
                .map(String::from)
                .unwrap_or_default();
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

    /// Publish arbitrary JSON to a subject that is bound to the runtime release JetStream stream.
    async fn publish_json<T: Serialize>(
        &self,
        subject: &str,
        payload: &T,
    ) -> Result<(), RuntimeNatsPublishError> {
        let bytes = serde_json::to_vec(payload).map_err(RuntimeNatsPublishError::Serialize)?;
        let payload_json = String::from_utf8_lossy(&bytes);
        tracing::info!(
            target: "runtime_nats",
            subject = %subject,
            payload = %payload_json,
            "publishing runtime release deploy event via JetStream (for runtimes to consume)"
        );
        self.jetstream
            .publish(subject.to_string(), bytes.into())
            .await
            .map_err(|e| RuntimeNatsPublishError::Publish(e.to_string()))?;
        tracing::info!(
            target: "runtime_nats",
            subject = %subject,
            "runtime release deploy event published via JetStream successfully"
        );
        Ok(())
    }

    pub async fn publish_stage_release(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
        deployed_revision: i32,
    ) -> Result<(), RuntimeNatsPublishError> {
        let payload = ReleaseDeployEvent {
            project_id,
            environment: ReleaseDeployEnvironment::Stage,
            runtime_id: Some(runtime_id),
            deployed_revision: Some(deployed_revision),
        };
        self.publish_json(subject, &payload).await
    }

    /// Stage runtime no longer has a live release revision (detach). Same subject as deploy events.
    pub async fn publish_stage_release_cleared(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
    ) -> Result<(), RuntimeNatsPublishError> {
        self.publish_stage_runtime_release_state(subject, project_id, runtime_id, None)
            .await
    }

    /// `Some(revision)` = stage deploy; `None` = detached / no live release.
    pub async fn publish_stage_runtime_release_state(
        &self,
        subject: &str,
        project_id: Uuid,
        runtime_id: Uuid,
        deployed_revision: Option<i32>,
    ) -> Result<(), RuntimeNatsPublishError> {
        match deployed_revision {
            Some(r) => self.publish_stage_release(subject, project_id, runtime_id, r).await,
            None => {
                tracing::info!(
                    target: "runtime_nats",
                    subject = %subject,
                    project_id = %project_id,
                    runtime_id = %runtime_id,
                    "publishing stage live release CLEARED (detach): deployed_revision=null for runtime consumer"
                );
                let payload = ReleaseDeployEvent {
                    project_id,
                    environment: ReleaseDeployEnvironment::Stage,
                    runtime_id: Some(runtime_id),
                    deployed_revision: None,
                };
                self.publish_json(subject, &payload).await
            }
        }
    }

    pub async fn publish_prod_release(
        &self,
        subject: &str,
        project_id: Uuid,
        deployed_revision: i32,
    ) -> Result<(), RuntimeNatsPublishError> {
        let payload = ReleaseDeployEvent {
            project_id,
            environment: ReleaseDeployEnvironment::Production,
            runtime_id: None,
            deployed_revision: Some(deployed_revision),
        };
        self.publish_json(subject, &payload).await
    }
}
