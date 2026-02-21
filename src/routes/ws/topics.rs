use diesel::prelude::*;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicKind {
    ChatSession {
        user_id: i64,
        project_id: i64,
        chat_session_id: i64,
    },
    NotificationsUser { user_id: i64 },
    User { user_id: i64 },
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
                let user_id = i64::from_str(parts[1]).ok()?;
                let project_id = i64::from_str(parts[2]).ok()?;
                let chat_session_id = i64::from_str(parts[3]).ok()?;
                Some(TopicKind::ChatSession {
                    user_id,
                    project_id,
                    chat_session_id,
                })
            }
            "notifications" => {
                if parts.len() != 3 || parts[1] != "user" {
                    return None;
                }
                let user_id = i64::from_str(parts[2]).ok()?;
                Some(TopicKind::NotificationsUser { user_id })
            }
            "user" => {
                if parts.len() != 2 {
                    return None;
                }
                let user_id = i64::from_str(parts[1]).ok()?;
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
            } => format!("chat_session:{}:{}:{}", user_id, project_id, chat_session_id),
            TopicKind::NotificationsUser { user_id } => format!("notifications:user:{}", user_id),
            TopicKind::User { user_id } => format!("user:{}", user_id),
        }
    }
}

pub enum TopicAuth {
    Allowed(String),
    Denied(&'static str),
}

/// Authorize a topic for the authenticated user. Called only on Subscribe/Unsubscribe, not per message.
/// DB lookups here are acceptable; for very high subscribe rates consider caching or moving the query into `web::block`.
pub fn authorize_topic(
    topic: &str,
    authenticated_user_id: i64,
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
            if *user_id != authenticated_user_id {
                return TopicAuth::Denied("topic user_id does not match authenticated user");
            }
            let mut conn = match pool.get() {
                Ok(c) => c,
                Err(_) => return TopicAuth::Denied("database unavailable"),
            };
            let exists: Option<i64> = crate::schema::chat_sessions::table
                .inner_join(
                    crate::schema::projects::table
                        .on(crate::schema::chat_sessions::project_id.eq(crate::schema::projects::id)),
                )
                .filter(crate::schema::projects::owner_user_id.eq(authenticated_user_id))
                .filter(crate::schema::chat_sessions::project_id.eq(project_id))
                .filter(crate::schema::chat_sessions::id.eq(chat_session_id))
                .select(crate::schema::chat_sessions::id)
                .first::<i64>(&mut conn)
                .optional()
                .ok()
                .flatten();
            match exists {
                Some(_) => TopicAuth::Allowed(kind.to_topic_string()),
                None => TopicAuth::Denied("chat session not found or access denied"),
            }
        }
        TopicKind::NotificationsUser { user_id } => {
            if *user_id != authenticated_user_id {
                return TopicAuth::Denied("topic user_id does not match authenticated user");
            }
            TopicAuth::Denied("notifications:user topic not implemented yet")
        }
        TopicKind::User { user_id } => {
            if *user_id != authenticated_user_id {
                return TopicAuth::Denied("topic user_id does not match authenticated user");
            }
            TopicAuth::Allowed(kind.to_topic_string())
        }
    }
}
