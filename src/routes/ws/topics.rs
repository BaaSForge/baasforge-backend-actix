//! WebSocket topic authorization.
//!
//! Users may only subscribe to their own user channel (`user:{their_user_id}`).
//! Subscribing to another user's channel is denied so users cannot receive other users' events.

use diesel::prelude::*;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicKind {
    ChatSession {
        user_id: Uuid,
        project_id: Uuid,
        chat_session_id: Uuid,
    },
    User {
        user_id: Uuid,
    },
}

impl TopicKind {
    pub fn parse(topic: &str) -> Option<Self> {
        let parts: Vec<&str> = topic.split(':').collect();
        if parts.is_empty() {
            return None;
        }
        match parts[0] {
            "chat_session" => {
                if parts.len() != 4 {
                    return None;
                }
                let user_id = Uuid::parse_str(parts[1]).ok()?;
                let project_id = Uuid::parse_str(parts[2]).ok()?;
                let chat_session_id = Uuid::parse_str(parts[3]).ok()?;
                Some(TopicKind::ChatSession {
                    user_id,
                    project_id,
                    chat_session_id,
                })
            }
            "user" => {
                if parts.len() != 2 {
                    return None;
                }
                let user_id = Uuid::parse_str(parts[1]).ok()?;
                Some(TopicKind::User { user_id })
            }
            _ => None,
        }
    }

    pub fn to_topic_string(&self) -> String {
        match self {
            TopicKind::ChatSession {
                user_id,
                project_id,
                chat_session_id,
            } => format!(
                "chat_session:{}:{}:{}",
                user_id, project_id, chat_session_id
            ),
            TopicKind::User { user_id } => format!("user:{}", user_id),
        }
    }
}

pub enum TopicAuth {
    Allowed(String),
    Denied(&'static str),
}

/// Authorizes a subscription: only the authenticated user's own channel is allowed.
/// Users cannot subscribe to other users' channels and therefore cannot receive other users' events.
pub async fn authorize_topic_async(
    topic: &str,
    authenticated_user_id: Uuid,
    pool: &crate::database::DbPool,
) -> TopicAuth {
    let kind = match TopicKind::parse(topic) {
        Some(k) => k,
        None => return TopicAuth::Denied("invalid topic format"),
    };

    match &kind {
        TopicKind::ChatSession {
            user_id,
            project_id,
            chat_session_id,
        } => {
            // Must not subscribe to another user's channel.
            if *user_id != authenticated_user_id {
                return TopicAuth::Denied("cannot subscribe to another user's channel");
            }
            let project_id = *project_id;
            let chat_session_id = *chat_session_id;
            let user_id = authenticated_user_id;
            match crate::database::block_on_db(pool, move |conn| {
                crate::schema::chat_sessions::table
                    .inner_join(crate::schema::projects::table.on(
                        crate::schema::chat_sessions::project_id.eq(crate::schema::projects::id),
                    ))
                    .filter(crate::schema::projects::owner_user_id.eq(user_id))
                    .filter(crate::schema::chat_sessions::project_id.eq(project_id))
                    .filter(crate::schema::chat_sessions::id.eq(chat_session_id))
                    .select(crate::schema::chat_sessions::id)
                    .first::<Uuid>(conn)
                    .optional()
            })
            .await
            {
                Ok(Some(_)) => TopicAuth::Allowed(kind.to_topic_string()),
                Ok(None) | Err(_) => TopicAuth::Denied("chat session not found or access denied"),
            }
        }
        TopicKind::User { user_id } => {
            // Users may only subscribe to their own user channel; cannot receive other users' events.
            if *user_id != authenticated_user_id {
                return TopicAuth::Denied("cannot subscribe to another user's channel");
            }
            TopicAuth::Allowed(kind.to_topic_string())
        }
    }
}
