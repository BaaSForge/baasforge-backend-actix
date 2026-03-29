use serde::{Deserialize, Serialize};
use std::fmt;
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

/// Shared [`async_nats::Client`]: one TCP connection and protocol state, publish to **any** subject per call.
pub struct RuntimeNatsPublisher {
    client: async_nats::Client,
}

impl RuntimeNatsPublisher {
    pub async fn connect(url: &str) -> Result<Self, async_nats::Error> {
        let client = async_nats::connect(url).await?;
        Ok(Self { client })
    }

    /// Publish arbitrary JSON to any subject (e.g. `runtime.release`, `alerts.prod`, …).
    async fn publish_json<T: Serialize>(
        &self,
        subject: &str,
        payload: &T,
    ) -> Result<(), RuntimeNatsPublishError> {
        let bytes = serde_json::to_vec(payload).map_err(RuntimeNatsPublishError::Serialize)?;
        self.client
            .publish(subject.to_string(), bytes.into())
            .await
            .map_err(|e| RuntimeNatsPublishError::Publish(e.to_string()))?;
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
