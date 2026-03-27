//! After the planner finishes a turn (no further LLM continuation), a queue probe runs so we can
//! append a `release_controller_box` message when the turn included unit create/modify/delete tools.

use crate::database::{block_on_db_tokio, DbPool};
use crate::libs::ids::new_uuid;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
    ChatSessionRoleType, ReleaseControllerBoxPayload,
};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{ChatSessionMessageCreatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Enqueue work so the next consumer may insert [`ChatMessageType::ReleaseControllerBox`].
pub async fn publish_release_controller_probe(
    rabbit_publisher: &Arc<SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) {
    let response_message_id = new_uuid();
    let queue_message = ChatSessionQueueMessage {
        user_id,
        role: ChatSessionRoleType::System,
        type_: ChatMessageType::ReleaseControllerBox,
        payload: None,
        response_message_id,
        retry_num: 0,
        invoking_message_id: None,
        agent: ChatAgentType::default(),
    };
    if let Err(e) = chat_message_queue::publish_with_publisher(
        rabbit_publisher,
        project_id,
        session_id,
        &queue_message,
    )
    .await
    {
        tracing::error!(
            %project_id,
            %session_id,
            %response_message_id,
            "publish_release_controller_probe: failed to publish: {}",
            e
        );
    } else {
        tracing::info!(
            %project_id,
            %session_id,
            %response_message_id,
            "Published release_controller_box queue probe"
        );
    }
}

/// Inserts the UI message when the last user → pending tail includes unit CRUD tool rows.
pub async fn consume_release_controller_queue_message(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    response_message_id: Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_id_db = session_id;

    let last_user = block_on_db_tokio(pool, move |conn| {
        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id_db))
            .filter(chat_messages::role.eq(ChatSessionRoleType::User))
            .order((chat_messages::created_at.desc(), chat_messages::id.desc()))
            .select(ChatMessageRow::as_select())
            .first(conn)
            .optional()
    })
    .await
    .map_err(|e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(e)
    })?;

    let Some(last_user) = last_user else {
        return Ok(());
    };

    let last_user_created = last_user.created_at;
    let last_user_id = last_user.id;

    let has_unit_crud = block_on_db_tokio(pool, move |conn| {
        let after_user = chat_messages::created_at
            .gt(last_user_created)
            .or(chat_messages::created_at
                .eq(last_user_created)
                .and(chat_messages::id.gt(last_user_id)));

        let n: i64 = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id_db))
            .filter(after_user)
            .filter(
                chat_messages::type_
                    .eq(ChatMessageType::ToolCreateUnit)
                    .or(chat_messages::type_.eq(ChatMessageType::ToolModifyUnit))
                    .or(chat_messages::type_.eq(ChatMessageType::ToolRemoveUnit)),
            )
            .count()
            .get_result(conn)?;
        Ok::<_, diesel::result::Error>(n > 0)
    })
    .await
    .map_err(|e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(e)
    })?;

    if !has_unit_crud {
        return Ok(());
    }

    let body = ReleaseControllerBoxPayload {
        stage_release: None,
        prod_release: None,
    };
    let payload_json = serde_json::to_value(&body)?;

    let session_id_ins = session_id;
    let inserted = block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(response_message_id),
                chat_messages::chat_session_id.eq(session_id_ins),
                chat_messages::role.eq(ChatSessionRoleType::System),
                chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox),
                chat_messages::payload.eq(Some(payload_json)),
                chat_messages::status.eq(ChatMessageStatus::Completed),
                chat_messages::invoking_message_id.eq(None::<Uuid>),
                chat_messages::agent.eq(None::<ChatAgentType>),
            ))
            .on_conflict(chat_messages::id)
            .do_nothing()
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
            .optional()
    })
    .await
    .map_err(|e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(e)
    })?;

    let Some(row) = inserted else {
        return Ok(());
    };

    let channel = user_channel(user_id);
    let message = ChatMessageResponse::from(row);
    let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
        project_id,
        session_id,
        message,
    });
    let mut guard = redis_conn.lock().await;
    let _ = publish_event_to_channel(&mut *guard, &channel, &event).await;
    Ok(())
}
