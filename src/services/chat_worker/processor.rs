use crate::configs::LlmProvider;
use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{
    ChatMessageInsert, ChatMessageType, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_previous_session_messages;
use crate::services::chat_worker::llm::{call_gemini_stream, call_gpt_stream};
use diesel::prelude::*;
use openai::Credentials;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

pub async fn process_one(
    pool: &DbPool,
    llm_provider: LlmProvider,
    openai_credentials: Option<&Credentials>,
    openai_model: &str,
    gemini_api_key: Option<&str>,
    gemini_model: &str,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    user_id: i64,
    project_id: i64,
    session_id: i64,
    user_text: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_messages = block_on_db_tokio(pool, move |conn| {
        load_previous_session_messages(conn, session_id, 50)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    let ai_text = match llm_provider {
        LlmProvider::OpenAi => {
            let credentials = openai_credentials.ok_or_else(|| {
                Box::<dyn std::error::Error + Send + Sync>::from("OPENAI credentials not set")
            })?;
            call_gpt_stream(
                credentials,
                openai_model,
                &session_messages,
                user_text,
                Some(redis_conn.clone()),
                &topic,
            )
            .await?
        }
        LlmProvider::Gemini => {
            let api_key = gemini_api_key.ok_or_else(|| {
                Box::<dyn std::error::Error + Send + Sync>::from("GEMINI_API_KEY not set")
            })?;
            call_gemini_stream(
                api_key,
                gemini_model,
                &session_messages,
                user_text,
                Some(redis_conn.clone()),
                &topic,
            )
            .await?
        }
    };

    let insert = ChatMessageInsert {
        chat_session_id: session_id,
        role: ChatSessionRoleType::Ai,
        type_: ChatMessageType::Text,
        payload: Some(serde_json::to_value(ai_text.clone())?),
    };

    block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values(&insert)
            .execute(conn)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    info!(project_id, session_id, "Processed and saved AI response");
    Ok(())
}

