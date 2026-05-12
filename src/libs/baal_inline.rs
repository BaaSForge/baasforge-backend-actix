//! Decode and rewrite `<baal:inline type="…" data="…"/>` widgets embedded in chat text.
//!
//! `data` is standard base64-encoded JSON. Supported `type` values: `release`, `trace`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;
use serde::Deserialize;
use std::borrow::Cow;
use std::sync::LazyLock;
use uuid::Uuid;

static BAAL_INLINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"<baal:inline(?:\s+type="(?P<type1>[^"]+)"\s+data="(?P<data1>[^"]*)"|\s+data="(?P<data2>[^"]*)"\s+type="(?P<type2>[^"]+)")\s*/>"#,
    )
    .expect("static baal:inline regex")
});

#[derive(Debug, Clone, PartialEq)]
pub enum BaalInlineDecoded {
    Release {
        id: Uuid,
        revision: f64,
        is_stage: bool,
    },
    Trace {
        trace_id: String,
        release_id: Uuid,
        revision: f64,
        is_stage: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaalInlineDecodeError {
    Base64(String),
    Json(String),
}

fn decode_base64_standard_padded(data_base64: &str) -> Result<Vec<u8>, BaalInlineDecodeError> {
    let mut s = data_base64.trim().to_string();
    let pad = s.len() % 4;
    if pad != 0 {
        s.reserve(4 - pad);
        for _ in 0..(4 - pad) {
            s.push('=');
        }
    }
    STANDARD
        .decode(s.as_bytes())
        .map_err(|e| BaalInlineDecodeError::Base64(e.to_string()))
}

/// Decode the `data` attribute for a `type` tag value (`release`, `trace`, …).
pub fn decode_baal_inline(
    type_attr: &str,
    data_base64: &str,
) -> Result<Option<BaalInlineDecoded>, BaalInlineDecodeError> {
    let bytes = decode_base64_standard_padded(data_base64)?;
    let json_str =
        std::str::from_utf8(&bytes).map_err(|e| BaalInlineDecodeError::Base64(e.to_string()))?;

    match type_attr {
        "release" => {
            let p: InlineReleaseJson = serde_json::from_str(json_str)
                .map_err(|e| BaalInlineDecodeError::Json(e.to_string()))?;
            Ok(Some(BaalInlineDecoded::Release {
                id: p.id,
                revision: p.revision,
                is_stage: p.is_stage,
            }))
        }
        "trace" => {
            let p: InlineTraceJson = serde_json::from_str(json_str)
                .map_err(|e| BaalInlineDecodeError::Json(e.to_string()))?;
            Ok(Some(BaalInlineDecoded::Trace {
                trace_id: p.trace_id,
                release_id: p.release.id,
                revision: p.release.revision,
                is_stage: p.release.is_stage,
            }))
        }
        _ => Ok(None),
    }
}

/// Human-readable token for chat session titles (and similar UI), e.g. `@release: stage 1.1`.
pub fn format_baal_inline_for_chat_title(decoded: &BaalInlineDecoded) -> String {
    match decoded {
        BaalInlineDecoded::Release {
            revision,
            is_stage,
            ..
        } => {
            format!(
                "@release: {} {}",
                release_environment_word(*is_stage),
                format_revision(*revision)
            )
        }
        BaalInlineDecoded::Trace {
            trace_id,
            revision,
            is_stage,
            ..
        } => {
            format!(
                "@trace: {} {} #{}",
                release_environment_word(*is_stage),
                format_revision(*revision),
                trace_id.trim()
            )
        }
    }
}

/// Compact bracket form for LLM consumption, e.g. `[release id: …, release environment: …]`.
pub fn format_baal_inline_for_llm(decoded: &BaalInlineDecoded) -> String {
    match decoded {
        BaalInlineDecoded::Release { id, is_stage, .. } => {
            format!(
                "[release id: {}, release environment: {}]",
                id,
                release_environment_word(*is_stage)
            )
        }
        BaalInlineDecoded::Trace { trace_id, .. } => {
            format!("[trace id: {}]", trace_id.trim())
        }
    }
}

/// Replace every `<baal:inline …/>` in `text` using `format_one`. Unrecognized or invalid tags are left unchanged.
pub fn replace_baal_inline_tags(text: &str, format_one: impl Fn(&BaalInlineDecoded) -> String) -> Cow<'_, str> {
    if !text.contains("<baal:inline") {
        return Cow::Borrowed(text);
    }
    let mut out: Option<String> = None;
    let mut last = 0usize;
    for caps in BAAL_INLINE_RE.captures_iter(text) {
        let m = caps.get(0).expect("full match");
        let (type_attr, data_b64) = inline_type_and_data(&caps);
        let replacement = decode_baal_inline(type_attr, data_b64)
            .ok()
            .flatten()
            .map(|d| format_one(&d));
        let buf = out.get_or_insert_with(|| String::with_capacity(text.len()));
        buf.push_str(&text[last..m.start()]);
        match replacement {
            Some(rep) => buf.push_str(&rep),
            None => buf.push_str(m.as_str()),
        }
        last = m.end();
    }
    match out {
        None => Cow::Borrowed(text),
        Some(mut buf) => {
            buf.push_str(&text[last..]);
            Cow::Owned(buf)
        }
    }
}

/// Expand inline widgets for chat session titles and word-based truncation.
pub fn expand_baal_inlines_for_chat_title(text: &str) -> String {
    match replace_baal_inline_tags(text, |d| format_baal_inline_for_chat_title(d)) {
        Cow::Borrowed(s) => s.to_string(),
        Cow::Owned(s) => s,
    }
}

/// Expand inline widgets in user-authored text before sending to an LLM.
pub fn expand_baal_inlines_for_llm_user_text(text: &str) -> String {
    match replace_baal_inline_tags(text, |d| format_baal_inline_for_llm(d)) {
        Cow::Borrowed(s) => s.to_string(),
        Cow::Owned(s) => s,
    }
}

fn inline_type_and_data<'a>(caps: &'a regex::Captures<'a>) -> (&'a str, &'a str) {
    if let Some(t) = caps.name("type1") {
        let d = caps.name("data1").map(|m| m.as_str()).unwrap_or("");
        return (t.as_str(), d);
    }
    let t = caps.name("type2").map(|m| m.as_str()).unwrap_or("");
    let d = caps.name("data2").map(|m| m.as_str()).unwrap_or("");
    (t, d)
}

fn release_environment_word(is_stage: bool) -> &'static str {
    if is_stage {
        "stage"
    } else {
        "production"
    }
}

fn format_revision(rev: f64) -> String {
    if rev.is_finite() && rev.fract() == 0.0 {
        format!("{}", rev as i64)
    } else {
        // Trim trailing zeros from fractional part for stable display.
        let s = format!("{rev}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[derive(Debug, Deserialize)]
struct InlineReleaseJson {
    id: Uuid,
    revision: f64,
    is_stage: bool,
}

#[derive(Debug, Deserialize)]
struct InlineTraceJson {
    release: InlineReleaseJson,
    #[serde(alias = "traceID", alias = "trace_id")]
    trace_id: String,
}
