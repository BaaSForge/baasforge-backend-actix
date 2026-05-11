//! `publish_reject_controller_box_probe` runs from the release-controller queue consumer after the
//! release row is written so the reject probe is always enqueued after that step.

use crate::database::{DbPool, PooledConn, block_on_db_tokio};
use crate::libs::ids::new_uuid;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus,
    ChatMessageType, ChatSessionRoleType, RejectControllerBoxPayload,
};
use crate::models::projects::ProjectUnitResponse;
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{ChatSessionMessageCreatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::message_queue::SharedRabbitPublisher;
use crate::services::releases::{LogicalUnitKey, logical_unit_key};
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub async fn publish_reject_controller_box_probe(
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
        type_: ChatMessageType::RejectControllerBox,
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
            "publish_reject_controller_box_probe: failed to publish: {}",
            e
        );
    } else {
        tracing::info!(
            %project_id,
            %session_id,
            %response_message_id,
            "Published reject_controller_box queue probe"
        );
    }
}

fn unit_changes_from_tool_rows(rows: &[ChatMessageRow]) -> Vec<ProjectUnitResponse> {
    let mut by_path: HashMap<LogicalUnitKey, ProjectUnitResponse> = HashMap::new();
    for row in rows {
        let Ok(Some(payload)) = ChatMessagePayload::from_value(&row.type_, row.payload.clone())
        else {
            continue;
        };
        let unit = match payload {
            ChatMessagePayload::ToolCreateUnit(p) => p.created_unit,
            ChatMessagePayload::ToolModifyUnit(p) => p.revised_unit,
            ChatMessagePayload::ToolRemoveUnit(p) => p.removed_unit,
            _ => None,
        };
        if let Some(u) = unit {
            let k = logical_unit_key(&u);
            by_path.insert(k, u);
        }
    }
    let mut out: Vec<_> = by_path.into_values().collect();
    out.sort_by(|a, b| {
        let (ca, ma, ta, ua) = logical_unit_key(a);
        let (cb, mb, tb, ub) = logical_unit_key(b);
        ca.cmp(&cb)
            .then_with(|| ma.cmp(&mb))
            .then_with(|| ta.as_slug().cmp(tb.as_slug()))
            .then_with(|| ua.cmp(&ub))
    });
    out
}

pub async fn consume_reject_controller_box_queue_message(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
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

    let tool_rows = block_on_db_tokio(pool, move |conn| {
        let after_user =
            chat_messages::created_at
                .gt(last_user_created)
                .or(chat_messages::created_at
                    .eq(last_user_created)
                    .and(chat_messages::id.gt(last_user_id)));

        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id_db))
            .filter(after_user)
            .filter(
                chat_messages::type_
                    .eq(ChatMessageType::ToolCreateUnit)
                    .or(chat_messages::type_.eq(ChatMessageType::ToolModifyUnit))
                    .or(chat_messages::type_.eq(ChatMessageType::ToolRemoveUnit)),
            )
            .order((chat_messages::created_at.asc(), chat_messages::id.asc()))
            .select(ChatMessageRow::as_select())
            .load::<ChatMessageRow>(conn)
    })
    .await
    .map_err(
        |e: crate::database::BlockingDbError| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(e)
        },
    )?;

    if tool_rows.is_empty() {
        return Ok(());
    }

    let unit_changes = unit_changes_from_tool_rows(&tool_rows);
    let body = RejectControllerBoxPayload {
        is_rejected: false,
        unit_changes,
        assistant_turn_id,
        is_disabled_due_to_live_production_release: false,
    };
    let payload_json = serde_json::to_value(&body)?;

    let session_id_ins = session_id;
    let inserted = block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(response_message_id),
                chat_messages::chat_session_id.eq(session_id_ins),
                chat_messages::role.eq(ChatSessionRoleType::System),
                chat_messages::type_.eq(ChatMessageType::RejectControllerBox),
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
    let mut guard = redis_conn.lock().await;
    let _ = publish_event_to_channel(&mut *guard, &channel, &event).await;
    Ok(())
}

pub fn apply_disabled_due_to_live_production_release_on_matching_reject_box(
    conn: &mut PooledConn,
    session_id: Uuid,
    assistant_turn_id: Uuid,
) -> Result<Option<ChatMessageRow>, diesel::result::Error> {
    let rows: Vec<ChatMessageRow> = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(ChatMessageType::RejectControllerBox))
        .select(ChatMessageRow::as_select())
        .load(conn)?;

    for row in rows {
        let Ok(Some(ChatMessagePayload::RejectControllerBox(mut p))) =
            ChatMessagePayload::from_value(&row.type_, row.payload.clone())
        else {
            continue;
        };
        if p.assistant_turn_id != Some(assistant_turn_id) {
            continue;
        }
        if p.is_disabled_due_to_live_production_release {
            return Ok(Some(row));
        }
        p.is_disabled_due_to_live_production_release = true;
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
