use serde_json::{json, Value};

use super::ingestion_events_to_otel;
use crate::types::IngestionEvent;

fn event(kind: &str, body: Value) -> IngestionEvent {
    serde_json::from_value(json!({
        "type": kind,
        "id": "envelope-id",
        "timestamp": "1970-01-01T00:00:01Z",
        "body": body
    }))
    .unwrap()
}

fn wire(events: &[IngestionEvent]) -> Value {
    serde_json::to_value(ingestion_events_to_otel(events)).unwrap()
}

fn spans(wire: &Value) -> &[Value] {
    wire["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap()
}

fn attrs(span: &Value) -> Vec<(&str, &Value)> {
    span["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|attr| (attr["key"].as_str().unwrap(), &attr["value"]))
        .collect()
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    attrs(span)
        .into_iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

fn common_body() -> Value {
    json!({
        "id": "ob-s", "traceId": "tr-ace", "parentObservationId": "par-ent",
        "name": "observation", "startTime": "1970-01-01T00:00:02Z",
        "endTime": "1970-01-01T00:00:03Z", "input": {"q": 1}, "output": [2],
        "metadata": {"m": true}, "version": "v1", "environment": "test",
        "sessionId": "session", "level": "ERROR", "statusMessage": "failed"
    })
}

#[test]
fn mixed_event_families_keep_create_update_and_parent_order() {
    // Deliberately interleave families: conversion must neither group by type nor
    // coalesce the same observation's Create/Update into a single span.
    let events = vec![
        event("trace-create", json!({"id": "tr-ace", "name": "trace"})),
        event(
            "span-create",
            json!({"id": "par-ent", "traceId": "tr-ace", "name": "parent-create"}),
        ),
        event(
            "generation-create",
            json!({"id": "gen-id", "traceId": "tr-ace", "parentObservationId": "par-ent", "name": "generation-create"}),
        ),
        event(
            "observation-create",
            json!({"id": "ob-s", "traceId": "tr-ace", "parentObservationId": "par-ent", "type": "TOOL", "name": "observation-create"}),
        ),
        event(
            "event-create",
            json!({"id": "ev-ent", "traceId": "tr-ace", "parentObservationId": "ob-s", "name": "event"}),
        ),
        event(
            "generation-update",
            json!({"id": "gen-id", "traceId": "tr-ace", "parentObservationId": "par-ent", "name": "generation-update"}),
        ),
        event(
            "score-create",
            json!({"id": "sc-ore", "traceId": "tr-ace", "observationId": "ob-s", "name": "quality", "value": 0.5}),
        ),
        event("sdk-log", json!({"log": {"message": "sdk"}})),
        event("session-create", json!({"id": "session"})),
        event(
            "observation-update",
            json!({"id": "ob-s", "traceId": "tr-ace", "parentObservationId": "par-ent", "type": "TOOL", "name": "observation-update"}),
        ),
        event(
            "span-update",
            json!({"id": "par-ent", "traceId": "tr-ace", "name": "parent-update"}),
        ),
        event("session-update", json!({"id": "session"})),
    ];
    let wire = wire(&events);
    let spans = spans(&wire);
    assert_eq!(spans.len(), 12);
    assert_eq!(
        spans
            .iter()
            .map(|span| span["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "trace",
            "parent-create",
            "generation-create",
            "observation-create",
            "event",
            "generation-update",
            "score:quality",
            "sdk-log",
            "session-create:session",
            "observation-update",
            "parent-update",
            "session-update:session"
        ]
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| span["spanId"].as_str())
            .collect::<Vec<_>>(),
        [
            Some("trace"),
            Some("parent"),
            Some("genid"),
            Some("obs"),
            Some("event"),
            Some("genid"),
            Some("score"),
            None,
            None,
            Some("obs"),
            Some("parent"),
            None
        ]
    );
    assert_eq!(spans[2]["parentSpanId"], "parent");
    assert_eq!(spans[4]["parentSpanId"], "obs");
    assert_eq!(spans[6]["parentSpanId"], "obs");
    assert_eq!(spans[0]["startTimeUnixNano"], "1000000000");
    assert_eq!(attr(&spans[0], "langfuse.observation.type"), None);
    assert_eq!(
        attr(&spans[3], "langfuse.observation.type"),
        Some(&json!({"stringValue": "tool"}))
    );
    assert_eq!(
        attr(&spans[7], "langfuse.sdk.log"),
        Some(&json!({"stringValue": "{\"message\":\"sdk\"}"}))
    );
}

#[test]
fn span_update_preserves_common_fields_but_omits_create_status_attribute() {
    let body = common_body();
    let wire = wire(&[
        event("span-create", body.clone()),
        event("span-update", body),
    ]);
    let spans = spans(&wire);
    let common = [
        (
            "langfuse.observation.input",
            json!({"stringValue": "{\"q\":1}"}),
        ),
        ("langfuse.observation.output", json!({"stringValue": "[2]"})),
        (
            "langfuse.observation.metadata",
            json!({"stringValue": "{\"m\":true}"}),
        ),
        ("langfuse.version", json!({"stringValue": "v1"})),
        ("langfuse.environment", json!({"stringValue": "test"})),
        ("langfuse.session.id", json!({"stringValue": "session"})),
    ];
    for span in spans {
        for (key, value) in &common {
            assert_eq!(attr(span, key), Some(value), "{key}");
        }
        assert_eq!(span["traceId"], "trace");
        assert_eq!(span["spanId"], "obs");
        assert_eq!(span["parentSpanId"], "parent");
        assert_eq!(span["startTimeUnixNano"], "2000000000");
        assert_eq!(span["endTimeUnixNano"], "3000000000");
        assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
    }
    assert_eq!(
        attr(&spans[0], "langfuse.observation.status_message"),
        Some(&json!({"stringValue": "failed"}))
    );
    assert_eq!(attr(&spans[1], "langfuse.observation.status_message"), None);
    // Attribute order differs between Create and Update and is wire-visible.
    assert_eq!(attrs(&spans[0])[1].0, "langfuse.observation.input");
    assert_eq!(attrs(&spans[1])[1].0, "langfuse.session.id");
}

#[test]
fn generation_update_keeps_model_and_usage_counters_without_create_only_fields() {
    let mut body = common_body();
    body.as_object_mut().unwrap().extend(
        json!({
            "model": "model", "modelParameters": {"temperature": 0.2},
            "usage": {"total": 7}, "usageDetails": {"input": 3},
            "costDetails": {"total": 0.1}, "promptName": "prompt", "promptVersion": 4,
            "completionStartTime": "1970-01-01T00:00:02.5Z"
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let wire = wire(&[
        event("generation-create", body.clone()),
        event("generation-update", body),
    ]);
    let spans = spans(&wire);
    for span in spans {
        assert_eq!(
            attr(span, "langfuse.observation.model.name"),
            Some(&json!({"stringValue": "model"}))
        );
        assert_eq!(
            attr(span, "gen_ai.usage.input"),
            Some(&json!({"intValue": 3}))
        );
        assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
    }
    for (key, value) in [
        (
            "langfuse.observation.model.parameters",
            "{\"temperature\":0.2}",
        ),
        ("langfuse.observation.usage_details", "{\"total\":7}"),
        ("langfuse.observation.cost_details", "{\"total\":0.1}"),
        ("langfuse.observation.prompt.name", "prompt"),
        (
            "langfuse.observation.completion_start_time",
            "1970-01-01T00:00:02.5Z",
        ),
    ] {
        assert_eq!(
            attr(&spans[0], key),
            Some(&json!({"stringValue": value})),
            "{key}"
        );
        assert_eq!(attr(&spans[1], key), None, "{key}");
    }
    assert_eq!(attr(&spans[0], "langfuse.observation.prompt.version"), None);
}

#[test]
fn observation_update_omits_model_and_status_attribute_but_keeps_error_status() {
    let mut body = common_body();
    body["type"] = json!("AGENT");
    body["model"] = json!("model");
    let wire = wire(&[
        event("observation-create", body.clone()),
        event("observation-update", body),
    ]);
    let spans = spans(&wire);
    for span in spans {
        assert_eq!(
            attr(span, "langfuse.observation.type"),
            Some(&json!({"stringValue": "agent"}))
        );
        assert_eq!(
            attr(span, "langfuse.session.id"),
            Some(&json!({"stringValue": "session"}))
        );
        assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
    }
    for key in [
        "langfuse.observation.model.name",
        "langfuse.observation.status_message",
    ] {
        assert!(attr(&spans[0], key).is_some());
        assert_eq!(attr(&spans[1], key), None);
    }
}

#[test]
fn metadata_events_keep_score_value_types_and_session_attributes() {
    let mut events = Vec::new();
    for value in [json!(2), json!(true), json!("good")] {
        events.push(event(
            "score-create",
            json!({"name": "quality", "value": value}),
        ));
    }
    let session = json!({"id": "session", "user_id": "user", "release": "release", "version": "v1", "source": "source", "metadata": {"m": 1}});
    events.push(event("session-create", session.clone()));
    events.push(event("session-update", session));
    let wire = wire(&events);
    let spans = spans(&wire);
    assert_eq!(
        attr(&spans[0], "langfuse.score.value"),
        Some(&json!({"doubleValue": 2.0}))
    );
    assert_eq!(
        attr(&spans[1], "langfuse.score.value"),
        Some(&json!({"boolValue": true}))
    );
    assert_eq!(
        attr(&spans[2], "langfuse.score.value"),
        Some(&json!({"stringValue": "\"good\""}))
    );
    assert_eq!(spans[3]["attributes"], spans[4]["attributes"]);
    assert_eq!(attrs(&spans[3]).len(), 6);
    for (key, value) in [
        ("langfuse.session.id", "session"),
        ("langfuse.user.id", "user"),
        ("langfuse.release", "release"),
        ("langfuse.version", "v1"),
        ("langfuse.session.source", "source"),
        ("langfuse.session.metadata", "{\"m\":1}"),
    ] {
        assert_eq!(attr(&spans[3], key), Some(&json!({"stringValue": value})));
    }
    assert!(spans[3].get("traceId").is_none());
    assert!(spans[3].get("spanId").is_none());
}

#[test]
fn missing_ids_invalid_times_and_event_status_keep_existing_wire_shape() {
    let wire = wire(&[
        event("trace-create", json!({"timestamp": "1970-01-01T00:00:02Z"})),
        event(
            "event-create",
            json!({"startTime": "invalid", "level": "WARNING", "statusMessage": "warning"}),
        ),
        event(
            "span-create",
            json!({"startTime": "1970-01-01T01:00:01+01:00", "endTime": "9999-12-31T23:59:59Z"}),
        ),
    ]);
    let spans = spans(&wire);
    assert_eq!(spans[0]["startTimeUnixNano"], "1000000000");
    assert_eq!(spans[0]["endTimeUnixNano"], "2000000000");
    for span in spans {
        assert_eq!(span["traceId"], "");
        assert_eq!(span["spanId"], "");
        assert!(span.get("parentSpanId").is_none());
        assert_eq!(span["status"], json!({}));
    }
    assert!(spans[1].get("startTimeUnixNano").is_none());
    assert!(spans[1].get("endTimeUnixNano").is_none());
    assert_eq!(spans[2]["startTimeUnixNano"], "1000000000");
    assert!(spans[2].get("endTimeUnixNano").is_none());
}

#[test]
fn empty_conversion_retains_single_resource_and_scope_envelope() {
    assert_eq!(
        wire(&[]),
        json!({"resourceSpans": [{
            "resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "peri-agent"}},
                {"key": "service.version", "value": {"stringValue": env!("CARGO_PKG_VERSION")}}
            ]},
            "scopeSpans": [{"scope": {"name": "langfuse-client", "version": env!("CARGO_PKG_VERSION")}, "spans": []}]
        }]})
    );
}

#[test]
fn trace_attributes_remain_distinct_from_observation_attributes() {
    let wire = wire(&[event(
        "trace-create",
        json!({
            "id": "tr-ace", "name": "trace", "sessionId": "session", "userId": "user",
            "release": "release", "version": "v1", "environment": "test", "tags": ["one", "two"],
            "input": {"q": 1}, "output": [2], "metadata": {"ignored": true}, "public": true
        }),
    )]);
    let span = &spans(&wire)[0];
    let expected = [
        ("langfuse.session.id", "session"),
        ("langfuse.user.id", "user"),
        ("langfuse.release", "release"),
        ("langfuse.version", "v1"),
        ("langfuse.environment", "test"),
        ("langfuse.trace.tags", "one,two"),
        ("langfuse.trace.input", "{\"q\":1}"),
        ("langfuse.trace.output", "[2]"),
        ("langfuse.trace.name", "trace"),
    ];
    assert_eq!(attrs(span).len(), expected.len());
    for ((actual_key, actual_value), (key, value)) in attrs(span).iter().zip(expected) {
        assert_eq!(*actual_key, key);
        assert_eq!(**actual_value, json!({"stringValue": value}));
    }
}

#[test]
fn event_common_attributes_and_error_status_do_not_introduce_an_end_time() {
    let mut body = common_body();
    body.as_object_mut().unwrap().remove("endTime");
    body.as_object_mut().unwrap().remove("sessionId");
    let wire = wire(&[event("event-create", body)]);
    let span = &spans(&wire)[0];
    assert_eq!(span["startTimeUnixNano"], "2000000000");
    assert!(span.get("endTimeUnixNano").is_none());
    assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
    assert_eq!(
        attr(span, "langfuse.observation.input"),
        Some(&json!({"stringValue": "{\"q\":1}"}))
    );
    assert_eq!(
        attr(span, "langfuse.observation.output"),
        Some(&json!({"stringValue": "[2]"}))
    );
    assert_eq!(
        attr(span, "langfuse.observation.metadata"),
        Some(&json!({"stringValue": "{\"m\":true}"}))
    );
    assert_eq!(attr(span, "langfuse.observation.status_message"), None);
}
