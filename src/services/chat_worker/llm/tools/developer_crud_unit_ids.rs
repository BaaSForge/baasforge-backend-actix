use crate::libs::unit_slug::normalize_full_unit_id_arg;
use crate::models::chat_messages::{ChatMessagePayload, ChatMessageRow};

pub(crate) fn push_normalized_full_unit_id_unique(out: &mut Vec<String>, id: impl AsRef<str>) {
    let normalized = normalize_full_unit_id_arg(id.as_ref());
    let t = normalized.trim();
    if t.is_empty() {
        return;
    }
    if !out.iter().any(|x| x == t) {
        out.push(t.to_string());
    }
}

fn extend_unique_unit_ids_from_crud_tool_payload(
    out: &mut Vec<String>,
    payload: &ChatMessagePayload,
) {
    match payload {
        ChatMessagePayload::ToolCreateUnit(p) if p.error.is_none() => {
            if let Some(u) = &p.created_unit {
                push_normalized_full_unit_id_unique(out, &u.id);
            }
        }
        ChatMessagePayload::ToolModifyUnit(p) if p.error.is_none() => {
            if let Some(u) = &p.revised_unit {
                push_normalized_full_unit_id_unique(out, &u.id);
            }
        }
        ChatMessagePayload::ToolRemoveUnit(p) if p.error.is_none() => {
            if let Some(u) = &p.removed_unit {
                push_normalized_full_unit_id_unique(out, &u.id);
            }
        }
        _ => {}
    }
}

pub(crate) fn extend_unique_unit_ids_from_chat_tool_row(out: &mut Vec<String>, row: &ChatMessageRow) {
    let Ok(Some(parsed)) = ChatMessagePayload::from_value(&row.type_, row.payload.clone()) else {
        return;
    };
    extend_unique_unit_ids_from_crud_tool_payload(out, &parsed);
}
