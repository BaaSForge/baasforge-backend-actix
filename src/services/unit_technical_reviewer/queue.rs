use crate::services::message_queue::{
    publish_with_channel, PublishError, SharedRabbitPublisher,
};
use lapin::{Connection, Consumer};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub const UNIT_REVIEWER_EXCHANGE: &str = "unit_reviewer_exchange";
pub const UNIT_REVIEWER_WORK_QUEUE: &str = "unit_reviewer_work_queue";
pub const UNIT_REVIEWER_WORK_QUEUE_BINDING: &str = "unit_reviewer.#";
pub const UNIT_REVIEWER_ROUTING_KEY: &str = "unit_reviewer.check";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitReviewerMessageType {
    CreateUnitCompilerCheck,
    CreateUnitRuleCheck,
    ModifyUnitCompilerCheck,
    ModifyUnitRuleCheck,
    RemoveUnitRuleCheck,
}

/// A message dispatched to the unit reviewer queue. The reviewer binary reads this,
/// runs the appropriate check, updates the tool message in DB, broadcasts WS, and when
/// all checks for a tool call are complete, triggers the LLM continuation via
/// `maybe_complete_invoking_message` in the chat worker queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitReviewerQueueMessage {
    pub message_type: UnitReviewerMessageType,
    /// ID of the original ToolCreateUnit / ToolModifyUnit / ToolRemoveUnit message row.
    pub tool_message_id: Uuid,
    /// User context: needed for WebSocket channel and WS events.
    pub user_id: Uuid,
    /// Session context: needed for WebSocket events and RabbitMQ routing key on callback.
    pub project_id: Uuid,
    pub session_id: Uuid,
    /// NodeJS code to check. Empty string for remove checks (no code to compile).
    pub code: String,
    /// Unit identity — needed for rule checks.
    pub unit_type: String,
    pub context_slug: String,
    pub module_slug: String,
    pub unit_name_slug: String,
}

pub async fn publish_reviewer_message(
    publisher: &Arc<SharedRabbitPublisher>,
    message: &UnitReviewerQueueMessage,
) -> Result<(), PublishError> {
    let payload = serde_json::to_vec(message).map_err(PublishError::Serialize)?;
    publish_with_channel(
        publisher.as_ref(),
        UNIT_REVIEWER_EXCHANGE,
        UNIT_REVIEWER_ROUTING_KEY,
        &payload,
    )
    .await
    .map_err(PublishError::Queue)
}

pub async fn create_reviewer_work_consumer_on_connection(
    connection: &Connection,
) -> Result<(lapin::Channel, Consumer), lapin::Error> {
    crate::services::message_queue::create_work_consumer_on_connection(
        connection,
        UNIT_REVIEWER_EXCHANGE,
        UNIT_REVIEWER_WORK_QUEUE,
        UNIT_REVIEWER_WORK_QUEUE_BINDING,
    )
    .await
}
