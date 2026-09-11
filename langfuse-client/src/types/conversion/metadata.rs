use super::{build_span_id, OtelAttribute, OtelAttributeValue, OtelSpan, OtelStatus};
use crate::types::{session::SessionBody, ScoreBody, SdkLogBody};

pub(super) fn score_create(body: &ScoreBody) -> OtelSpan {
    // Scores are attached via attributes on the trace
    let mut attrs = vec![];
    attrs.push(OtelAttribute::string("langfuse.score.name", &body.name));
    attrs.push(OtelAttribute::new(
        "langfuse.score.value",
        match &body.value {
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    OtelAttributeValue {
                        string_value: None,
                        int_value: None,
                        double_value: Some(f),
                        bool_value: None,
                    }
                } else if let Some(i) = n.as_i64() {
                    OtelAttributeValue::int(i)
                } else {
                    OtelAttributeValue::string(body.value.to_string())
                }
            }
            serde_json::Value::Bool(b) => OtelAttributeValue::bool(*b),
            _ => OtelAttributeValue::string(body.value.to_string()),
        },
    ));
    if let Some(ref trace_id) = body.trace_id {
        attrs.push(OtelAttribute::string("langfuse.trace.id", trace_id));
    }
    if let Some(ref obs_id) = body.observation_id {
        attrs.push(OtelAttribute::string("langfuse.observation.id", obs_id));
    }

    let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
    let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));

    OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id: body.observation_id.as_deref().map(build_span_id),
        name: Some(format!("score:{}", body.name)),
        kind: Some(1),
        start_time_unix_nano: None,
        end_time_unix_nano: None,
        attributes: Some(attrs),
        status: Some(OtelStatus::default()),
    }
}

pub(super) fn sdk_log(body: &SdkLogBody) -> OtelSpan {
    // SDK logs are metadata; we skip them in OTLP as there's no natural mapping
    let attrs = vec![OtelAttribute::string(
        "langfuse.sdk.log",
        body.log.to_string(),
    )];
    OtelSpan {
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        name: Some("sdk-log".into()),
        kind: Some(1),
        start_time_unix_nano: None,
        end_time_unix_nano: None,
        attributes: Some(attrs),
        status: Some(OtelStatus::default()),
    }
}

pub(super) fn session_create(body: &SessionBody) -> OtelSpan {
    let mut attrs = vec![OtelAttribute::string("langfuse.session.id", &body.id)];
    if let Some(ref user_id) = body.user_id {
        attrs.push(OtelAttribute::string("langfuse.user.id", user_id));
    }
    if let Some(ref release) = body.release {
        attrs.push(OtelAttribute::string("langfuse.release", release));
    }
    if let Some(ref version) = body.version {
        attrs.push(OtelAttribute::string("langfuse.version", version));
    }
    if let Some(ref source) = body.source {
        attrs.push(OtelAttribute::string("langfuse.session.source", source));
    }
    if let Some(ref metadata) = body.metadata {
        if let Ok(json) = serde_json::to_string(metadata) {
            attrs.push(OtelAttribute::string("langfuse.session.metadata", json));
        }
    }
    OtelSpan {
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        name: Some(format!("session-create:{}", body.id)),
        kind: Some(1),
        start_time_unix_nano: None,
        end_time_unix_nano: None,
        attributes: Some(attrs),
        status: Some(OtelStatus::default()),
    }
}

pub(super) fn session_update(body: &SessionBody) -> OtelSpan {
    let mut attrs = vec![OtelAttribute::string("langfuse.session.id", &body.id)];
    if let Some(ref user_id) = body.user_id {
        attrs.push(OtelAttribute::string("langfuse.user.id", user_id));
    }
    if let Some(ref release) = body.release {
        attrs.push(OtelAttribute::string("langfuse.release", release));
    }
    if let Some(ref version) = body.version {
        attrs.push(OtelAttribute::string("langfuse.version", version));
    }
    if let Some(ref source) = body.source {
        attrs.push(OtelAttribute::string("langfuse.session.source", source));
    }
    if let Some(ref metadata) = body.metadata {
        if let Ok(json) = serde_json::to_string(metadata) {
            attrs.push(OtelAttribute::string("langfuse.session.metadata", json));
        }
    }
    OtelSpan {
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        name: Some(format!("session-update:{}", body.id)),
        kind: Some(1),
        start_time_unix_nano: None,
        end_time_unix_nano: None,
        attributes: Some(attrs),
        status: Some(OtelStatus::default()),
    }
}
