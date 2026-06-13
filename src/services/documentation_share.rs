use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use ring::rand::{SecureRandom, SystemRandom};
use uuid::Uuid;

use crate::schema::projects::dsl::*;

pub const DOCUMENTATION_SHARE_SECRET_HEADER: &str = "x-share-secret";

fn documentation_share_keys_equal(stored_key: &str, provided_key: &str) -> bool {
    if stored_key.len() != provided_key.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in stored_key.bytes().zip(provided_key.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub fn generate_documentation_share_key() -> String {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .expect("system RNG must provide entropy");
    URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectDocumentationShareRow {
    pub documentation_share_key: Option<String>,
    pub documentation_share_key_expires_at: Option<DateTime<Utc>>,
}

pub fn is_documentation_share_key_valid(
    conn: &mut PgConnection,
    project_id: Uuid,
    provided_key: &str,
) -> Result<bool, diesel::result::Error> {
    let row = projects
        .filter(id.eq(project_id))
        .select(ProjectDocumentationShareRow::as_select())
        .first::<ProjectDocumentationShareRow>(conn)
        .optional()?;

    let Some(row) = row else {
        return Ok(false);
    };

    let Some(stored_key) = row.documentation_share_key else {
        return Ok(false);
    };

    if !documentation_share_keys_equal(&stored_key, provided_key) {
        return Ok(false);
    }

    match row.documentation_share_key_expires_at {
        None => Ok(true),
        Some(expires_at) if expires_at > Utc::now() => Ok(true),
        Some(_) => Ok(false),
    }
}
