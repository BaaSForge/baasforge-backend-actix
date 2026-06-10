//! After the planner finishes a turn (no further LLM continuation), a queue probe runs so we can
//! append a `release_controller_box` message when the turn included unit create/modify/delete tools.
//! When that probe is consumed and the release row is inserted, we enqueue the paired
//! `reject_controller_box` probe so it always follows the release step in the queue.

use crate::database::{DbPool, PooledConn, block_on_db_tokio};
use crate::libs::ids::new_uuid;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus,
    ChatMessageType, ChatSessionRoleType, ReleaseControllerBoxPayload,
};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{ChatSessionMessageCreatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::reject_controller_box;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use crate::services::redis_conn::RedisConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Enqueue work so the next consumer may insert [`ChatMessageType::ReleaseControllerBox`].
pub async fn publish_release_controller_probe(
    rabbit_publisher: &Arc<SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    assistant_turn_id: Uuid,
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
        sub_agent_stuck_continuation_count: 0,
        assistant_turn_id: Some(assistant_turn_id),
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

/// Inserts the UI message when the last user → pending tail includes unit CRUD tool rows, then
/// publishes the `reject_controller_box` queue message for the same `assistant_turn_id`.
pub async fn consume_release_controller_queue_message(
    pool: &DbPool,
    redis_conn: &crate::services::redis_conn::RedisConn,
    rabbit_publisher: &Arc<SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    response_message_id: Uuid,
    assistant_turn_id: Option<Uuid>,
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
    .map_err(
        |e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(e)
        },
    )?;

    let Some(last_user) = last_user else {
        return Ok(());
    };

    let last_user_created = last_user.created_at;
    let last_user_id = last_user.id;

    let has_unit_crud = block_on_db_tokio(pool, move |conn| {
        let after_user =
            chat_messages::created_at
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
    .map_err(
        |e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(e)
        },
    )?;

    if !has_unit_crud {
        return Ok(());
    }

    let body = ReleaseControllerBoxPayload {
        stage_release: None,
        prod_release: None,
        assistant_turn_id,
        is_disabled_due_to_reject: false,
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
    .map_err(
        |e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(e)
        },
    )?;

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
    let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
    let _ = publish_event_to_channel(&mut guard, &channel, &event).await;
    drop(guard);

    if let Some(tid) = assistant_turn_id {
        reject_controller_box::publish_reject_controller_box_probe(
            rabbit_publisher,
            user_id,
            project_id,
            session_id,
            tid,
        )
        .await;
    } else {
        tracing::warn!(
            %project_id,
            %session_id,
            %response_message_id,
            "release_controller_box: inserted row but assistant_turn_id missing; skip reject_controller probe"
        );
    }
    Ok(())
}

pub fn apply_is_disabled_due_to_reject_on_matching_release_box(
    conn: &mut PooledConn,
    session_id: Uuid,
    assistant_turn_id: Uuid,
) -> Result<Option<ChatMessageRow>, diesel::result::Error> {
    let rows: Vec<ChatMessageRow> = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
        .select(ChatMessageRow::as_select())
        .load(conn)?;

    for row in rows {
        let Ok(Some(ChatMessagePayload::ReleaseControllerBox(mut p))) =
            ChatMessagePayload::from_value(&row.type_, row.payload.clone())
        else {
            continue;
        };
        if p.assistant_turn_id != Some(assistant_turn_id) {
            continue;
        }
        if p.is_disabled_due_to_reject {
            return Ok(Some(row));
        }
        p.is_disabled_due_to_reject = true;
        let payload_json = match serde_json::to_value(&p) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let updated_row = diesel::update(chat_messages::table.filter(chat_messages::id.eq(row.id)))
            .set(chat_messages::payload.eq(Some(payload_json)))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)?;
        return Ok(Some(updated_row));
    }
    Ok(None)
}
