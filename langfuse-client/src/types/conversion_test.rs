use std::collections::HashSet;

use serde_json::{json, Value};

use super::ingestion_events_to_otel;
use crate::{error::LangfuseError, types::IngestionEvent};

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
    let request = ingestion_events_to_otel(events).unwrap();
    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap()
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

fn failure(events: &[IngestionEvent]) -> String {
    match ingestion_events_to_otel(events) {
        Err(LangfuseError::IngestionApi(message)) => message,
        result => panic!("expected a safe conversion error, got {result:?}"),
    }
}

fn assert_id(value: &Value, length: usize) {
    let identity = value.as_str().unwrap();
    assert_eq!(identity.len(), length);
    assert!(identity.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(identity, identity.to_ascii_lowercase());
    assert!(identity.bytes().any(|byte| byte != b'0'));
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

fn create_body(kind: &str) -> Value {
    let mut body = common_body();
    match kind {
        "observation-create" => body["type"] = json!("TOOL"),
        "event-create" => {
            body.as_object_mut().unwrap().remove("endTime");
            body.as_object_mut().unwrap().remove("sessionId");
        }
        _ => {}
    }
    body
}

#[test]
fn mixed_create_families_keep_order_and_valid_parent_references() {
    let wire = wire(&[
        event("trace-create", json!({"id": "tr-ace", "name": "trace"})),
        event("session-create", json!({"id": "session"})),
        event(
            "span-create",
            json!({"id": "par-ent", "traceId": "tr-ace", "parentObservationId": "tr-ace", "name": "parent"}),
        ),
        event(
            "generation-create",
            json!({"id": "gen-id", "traceId": "tr-ace", "parentObservationId": "par-ent", "name": "generation"}),
        ),
        event("sdk-log", json!({"log": {"message": "sdk"}})),
        event(
            "observation-create",
            json!({"id": "ob-s", "traceId": "tr-ace", "parentObservationId": "par-ent", "type": "TOOL", "name": "tool"}),
        ),
        event(
            "event-create",
            json!({"id": "ev-ent", "traceId": "tr-ace", "parentObservationId": "ob-s", "name": "event"}),
        ),
        event("session-update", json!({"id": "session"})),
    ]);
    let spans = spans(&wire);
    assert_eq!(spans.len(), 5);
    assert_eq!(
        spans
            .iter()
            .map(|span| span["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["trace", "parent", "generation", "tool", "event"]
    );
    for span in spans {
        assert_id(&span["traceId"], 32);
        assert_id(&span["spanId"], 16);
        assert_eq!(span["traceId"], spans[0]["traceId"]);
        assert_eq!(span["startTimeUnixNano"], "1000000000");
        assert_eq!(span["endTimeUnixNano"], "1000000000");
        if let Some(parent) = span.get("parentSpanId") {
            assert_id(parent, 16);
        }
    }
    assert!(spans[0].get("parentSpanId").is_none());
    assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
    assert_eq!(spans[2]["parentSpanId"], spans[1]["spanId"]);
    assert_eq!(spans[3]["parentSpanId"], spans[1]["spanId"]);
    assert_eq!(spans[4]["parentSpanId"], spans[3]["spanId"]);
    assert_eq!(attr(&spans[0], "langfuse.observation.type"), None);
    assert_eq!(
        attr(&spans[3], "langfuse.observation.type"),
        Some(&json!({"stringValue": "tool"}))
    );
}

#[test]
fn standard_ids_are_preserved_and_normalized_to_lowercase() {
    let wire = wire(&[event(
        "span-create",
        json!({
            "id": "AABBCCDDEEFF1234",
            "traceId": "AABBCCDDEEFF00112233445566778899",
            "parentObservationId": "FEDCBA9876543210"
        }),
    )]);
    let span = &spans(&wire)[0];
    assert_eq!(span["traceId"], "aabbccddeeff00112233445566778899");
    assert_eq!(span["spanId"], "aabbccddeeff1234");
    assert_eq!(span["parentSpanId"], "fedcba9876543210");
}

#[test]
fn standard_trace_root_and_child_parent_use_span_mapping_not_trace_mapping() {
    let trace_id = "AABBCCDDEEFF00112233445566778899";
    let wire = wire(&[
        event("trace-create", json!({"id": trace_id})),
        event(
            "span-create",
            json!({
                "id": "child", "traceId": trace_id, "parentObservationId": trace_id
            }),
        ),
    ]);
    let spans = spans(&wire);
    assert_eq!(spans[0]["traceId"], "aabbccddeeff00112233445566778899");
    assert_eq!(spans[1]["traceId"], spans[0]["traceId"]);
    assert_id(&spans[0]["spanId"], 16);
    assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
    assert_ne!(spans[1]["parentSpanId"], spans[0]["traceId"]);
}

#[test]
fn standard_and_uuid_root_parent_references_are_case_insensitive() {
    for root_id in [
        "AABBCCDDEEFF00112233445566778899",
        "0199AABB-CCDD-7000-8000-ABCDEF123456",
    ] {
        let wire = wire(&[
            event("trace-create", json!({"id": root_id})),
            event(
                "span-create",
                json!({
                    "id": "child", "traceId": root_id.to_ascii_lowercase(),
                    "parentObservationId": root_id.to_ascii_lowercase()
                }),
            ),
        ]);
        let spans = spans(&wire);
        assert_id(&spans[0]["spanId"], 16);
        assert_eq!(spans[1]["traceId"], spans[0]["traceId"]);
        assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
    }
}

#[test]
fn domain_id_hashes_are_deterministic_and_domain_separated() {
    let events = [
        event("trace-create", json!({"id": "trace_domain"})),
        event(
            "generation-create",
            json!({
                "id": "gen_domain", "traceId": "trace_domain", "parentObservationId": "trace_domain"
            }),
        ),
    ];
    let first = wire(&events);
    assert_eq!(first, wire(&events));
    let spans = spans(&first);
    assert_eq!(spans[0]["traceId"], "e7021500cd9468bb481759d32eac7464");
    assert_eq!(spans[0]["spanId"], "b1cee4287904711c");
    assert_eq!(spans[1]["spanId"], "b1e094be93c6aa60");
    assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
}

#[test]
fn uuid_suffixes_sharing_a_time_prefix_do_not_collide() {
    let events = (0..256)
        .map(|index| {
            event(
                "trace-create",
                json!({"id": format!("0199aabb-ccdd-7000-8000-{index:012x}")}),
            )
        })
        .collect::<Vec<_>>();
    let wire = wire(&events);
    let spans = spans(&wire);
    let mut trace_ids = HashSet::new();
    let mut span_ids = HashSet::new();
    for span in spans {
        assert_id(&span["traceId"], 32);
        assert_id(&span["spanId"], 16);
        assert!(trace_ids.insert(span["traceId"].as_str().unwrap()));
        assert!(span_ids.insert(span["spanId"].as_str().unwrap()));
    }
    assert_eq!(trace_ids.len(), 256);
    assert_eq!(span_ids.len(), 256);
}

#[test]
fn seeded_random_prefixed_ids_are_nonzero_unique_and_parent_consistent() {
    let mut seed = 0x3f54_18a9_663c_712du64;
    let mut events = Vec::new();
    for _ in 0..512 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let root = format!("turn_{seed:016x}");
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let child = format!("gen_{seed:016x}");
        events.push(event("trace-create", json!({"id": root})));
        events.push(event(
            "generation-create",
            json!({
                "id": child, "traceId": root, "parentObservationId": root
            }),
        ));
    }
    let first = wire(&events);
    assert_eq!(first, wire(&events));
    let mut trace_ids = HashSet::new();
    let mut span_ids = HashSet::new();
    for pair in spans(&first).as_chunks::<2>().0 {
        assert_id(&pair[0]["traceId"], 32);
        assert!(trace_ids.insert(pair[0]["traceId"].as_str().unwrap()));
        for span in pair {
            assert_id(&span["spanId"], 16);
            assert!(span_ids.insert(span["spanId"].as_str().unwrap()));
            assert_eq!(span["traceId"], pair[0]["traceId"]);
        }
        assert_id(&pair[1]["parentSpanId"], 16);
        assert_eq!(pair[1]["parentSpanId"], pair[0]["spanId"]);
    }
    assert_eq!(trace_ids.len(), 512);
    assert_eq!(span_ids.len(), 1024);
}

#[test]
fn distinct_domain_ids_do_not_collapse_when_dashes_are_removed() {
    let wire = wire(&[
        event("trace-create", json!({"id": "abc-def"})),
        event("trace-create", json!({"id": "abcdef"})),
    ]);
    let spans = spans(&wire);
    assert_ne!(spans[0]["traceId"], spans[1]["traceId"]);
    assert_ne!(spans[0]["spanId"], spans[1]["spanId"]);
}

#[test]
fn missing_trace_identity_is_rejected_instead_of_using_envelope_id() {
    for value in [Value::Null, json!(""), json!(" \t\n")] {
        assert_eq!(
            failure(&[event("trace-create", json!({"id": value}))]),
            "OTLP conversion: missing trace identity"
        );
    }
    assert_eq!(
        failure(&[event("trace-create", json!({}))]),
        "OTLP conversion: missing trace identity"
    );
}

#[test]
fn missing_observation_and_trace_ids_are_rejected_for_every_create_family() {
    for kind in [
        "span-create",
        "generation-create",
        "observation-create",
        "event-create",
    ] {
        for field in ["id", "traceId"] {
            let expected = if field == "id" {
                "missing span identity"
            } else {
                "missing trace identity"
            };
            for value in [None, Some(Value::Null), Some(json!("")), Some(json!(" \t"))] {
                let mut body = create_body(kind);
                body.as_object_mut().unwrap().remove(field);
                if let Some(value) = value {
                    body[field] = value;
                }
                assert_eq!(
                    failure(&[event(kind, body)]),
                    format!("OTLP conversion: {expected}"),
                    "{kind}/{field}"
                );
            }
        }
    }
}

#[test]
fn explicit_zero_ids_and_empty_parent_are_rejected() {
    for (field, value) in [
        ("traceId", "00000000000000000000000000000000"),
        ("id", "0000000000000000"),
        ("parentObservationId", "0000000000000000"),
    ] {
        let mut body = common_body();
        body[field] = json!(value);
        assert_eq!(
            failure(&[event("span-create", body)]),
            "OTLP conversion: zero identity is invalid"
        );
    }
    for value in ["", " \t"] {
        let mut body = common_body();
        body["parentObservationId"] = json!(value);
        assert_eq!(
            failure(&[event("span-create", body)]),
            "OTLP conversion: missing span identity"
        );
    }
}

#[test]
fn update_families_fail_entire_batch_without_exposing_event_contents() {
    for kind in ["span-update", "generation-update", "observation-update"] {
        let mut body = common_body();
        body["id"] = json!("private-observation-identity");
        body["input"] = json!({"token": "fixture-private-payload"});
        if kind == "observation-update" {
            body["type"] = json!("AGENT");
        }
        let message = failure(&[event("span-create", common_body()), event(kind, body)]);
        assert_eq!(
            message,
            "OTLP conversion: update events require a complete create event"
        );
        assert!(!message.contains("private"));
    }
}

#[test]
fn scores_are_explicitly_rejected_not_exported_as_spans() {
    for value in [json!(2), json!(true), json!("good")] {
        let message = failure(&[
            event("trace-create", json!({"id": "trace"})),
            event(
                "score-create",
                json!({
                    "id": "score", "traceId": "trace", "observationId": "observation",
                    "name": "fixture-private-score", "value": value
                }),
            ),
        ]);
        assert_eq!(
            message,
            "OTLP conversion: score events require the dedicated score API"
        );
        assert!(!message.contains("private"));
    }
}

#[test]
fn session_records_and_sdk_logs_never_produce_identityless_spans() {
    let session = json!({
        "id": "session", "user_id": "user", "release": "release", "version": "v1",
        "source": "source", "metadata": {"m": 1}
    });
    let wire = wire(&[
        event("session-create", session.clone()),
        event("session-update", session),
        event("sdk-log", json!({"log": {"message": "sdk"}})),
    ]);
    assert!(spans(&wire).is_empty());
}

#[test]
fn observation_create_preserves_common_fields_and_session_attribute() {
    let wire = wire(&[event("span-create", common_body())]);
    let span = &spans(&wire)[0];
    for (key, value) in [
        ("langfuse.observation.input", "{\"q\":1}"),
        ("langfuse.observation.output", "[2]"),
        ("langfuse.observation.metadata", "{\"m\":true}"),
        ("langfuse.observation.metadata.m", "true"),
        ("langfuse.version", "v1"),
        ("langfuse.environment", "test"),
        ("langfuse.session.id", "session"),
        ("langfuse.observation.status_message", "failed"),
    ] {
        assert_eq!(
            attr(span, key),
            Some(&json!({"stringValue": value})),
            "{key}"
        );
    }
    assert_eq!(span["startTimeUnixNano"], "2000000000");
    assert_eq!(span["endTimeUnixNano"], "3000000000");
    assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
}

#[test]
fn generation_create_preserves_model_usage_cost_prompt_and_session_fields() {
    let mut body = common_body();
    body.as_object_mut().unwrap().extend(
        json!({
            "model": "model", "modelParameters": {"temperature": 0.2},
            "usage": {"total": 7}, "usageDetails": {"input": 3}, "costDetails": {"total": 0.1},
            "promptName": "prompt", "promptVersion": 4,
            "completionStartTime": "1970-01-01T00:00:02.5Z"
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let wire = wire(&[event("generation-create", body)]);
    let span = &spans(&wire)[0];
    for (key, value) in [
        ("langfuse.observation.model.name", "model"),
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
        ("langfuse.session.id", "session"),
    ] {
        assert_eq!(
            attr(span, key),
            Some(&json!({"stringValue": value})),
            "{key}"
        );
    }
    assert_eq!(
        attr(span, "gen_ai.usage.input"),
        Some(&json!({"intValue": 3}))
    );
    assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
}

#[test]
fn typed_observation_preserves_model_status_and_session_fields() {
    let mut body = common_body();
    body["type"] = json!("AGENT");
    body["model"] = json!("model");
    let wire = wire(&[event("observation-create", body)]);
    let span = &spans(&wire)[0];
    for (key, value) in [
        ("langfuse.observation.type", "agent"),
        ("langfuse.observation.model.name", "model"),
        ("langfuse.observation.status_message", "failed"),
        ("langfuse.session.id", "session"),
    ] {
        assert_eq!(
            attr(span, key),
            Some(&json!({"stringValue": value})),
            "{key}"
        );
    }
    assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
}

#[test]
fn trace_attributes_remain_distinct_and_metadata_has_filterable_fields() {
    let wire = wire(&[event(
        "trace-create",
        json!({
            "id": "tr-ace", "name": "trace", "sessionId": "session", "userId": "user",
            "release": "release", "version": "v1", "environment": "test", "tags": ["one", "two"],
            "input": {"q": 1}, "output": [2], "metadata": {"category": "test"}, "public": true
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
        ("langfuse.trace.metadata", "{\"category\":\"test\"}"),
        ("langfuse.trace.metadata.category", "test"),
    ];
    assert_eq!(attrs(span).len(), expected.len());
    for ((actual_key, actual_value), (key, value)) in attrs(span).iter().zip(expected) {
        assert_eq!(*actual_key, key);
        assert_eq!(**actual_value, json!({"stringValue": value}));
    }
}

#[test]
fn metadata_blob_is_preserved_alongside_named_filterable_fields() {
    let mut body = common_body();
    let metadata = json!({"label": "tools", "count": 2, "nested": {"ok": true}, "items": [1, 2]});
    body["metadata"] = metadata.clone();
    let wire = wire(&[event("span-create", body)]);
    let span = &spans(&wire)[0];
    assert_eq!(
        attr(span, "langfuse.observation.metadata"),
        Some(&json!({"stringValue": metadata.to_string()}))
    );
    for (key, value) in [
        ("label", "tools"),
        ("count", "2"),
        ("nested", "{\"ok\":true}"),
        ("items", "[1,2]"),
    ] {
        assert_eq!(
            attr(span, &format!("langfuse.observation.metadata.{key}")),
            Some(&json!({"stringValue": value}))
        );
    }
}

#[test]
fn event_is_instantaneous_and_preserves_common_fields_and_error_status() {
    let wire = wire(&[event("event-create", create_body("event-create"))]);
    let span = &spans(&wire)[0];
    assert_eq!(span["startTimeUnixNano"], "2000000000");
    assert_eq!(span["endTimeUnixNano"], span["startTimeUnixNano"]);
    assert_eq!(span["status"], json!({"code": 2, "message": "failed"}));
    for (key, value) in [
        ("langfuse.observation.input", "{\"q\":1}"),
        ("langfuse.observation.output", "[2]"),
        ("langfuse.observation.metadata", "{\"m\":true}"),
    ] {
        assert_eq!(attr(span, key), Some(&json!({"stringValue": value})));
    }
}

#[test]
fn missing_end_time_completes_each_create_family_with_zero_duration() {
    for kind in [
        "span-create",
        "generation-create",
        "observation-create",
        "event-create",
    ] {
        let mut body = create_body(kind);
        body.as_object_mut().unwrap().remove("endTime");
        let wire = wire(&[event(kind, body)]);
        let span = &spans(&wire)[0];
        assert_eq!(span["startTimeUnixNano"], "2000000000", "{kind}");
        assert_eq!(span["endTimeUnixNano"], span["startTimeUnixNano"], "{kind}");
    }
}

#[test]
fn trace_body_timestamp_is_start_time_and_envelope_is_only_a_fallback() {
    let wire = wire(&[event(
        "trace-create",
        json!({
            "id": "trace", "timestamp": "1970-01-01T00:00:02Z"
        }),
    )]);
    let span = &spans(&wire)[0];
    assert_eq!(span["startTimeUnixNano"], "2000000000");
    assert_eq!(span["endTimeUnixNano"], "2000000000");
}

#[test]
fn invalid_explicit_times_fail_safely_instead_of_being_omitted() {
    for (field, value, expected) in [
        ("startTime", "invalid-private-date", "invalid start time"),
        ("endTime", "invalid-private-date", "invalid end time"),
        ("startTime", "1969-12-31T23:59:59Z", "invalid start time"),
        ("endTime", "9999-12-31T23:59:59Z", "invalid end time"),
        (
            "endTime",
            "1970-01-01T00:00:01Z",
            "end time precedes start time",
        ),
    ] {
        let mut body = common_body();
        body[field] = json!(value);
        assert_eq!(
            failure(&[event("span-create", body)]),
            format!("OTLP conversion: {expected}")
        );
    }
}

#[test]
fn invalid_envelope_timestamp_fails_when_used_as_start_fallback() {
    let mut event = event("span-create", json!({"id": "span", "traceId": "trace"}));
    match &mut event {
        IngestionEvent::SpanCreate { timestamp, .. } => *timestamp = "invalid-private-date".into(),
        _ => unreachable!(),
    }
    assert_eq!(failure(&[event]), "OTLP conversion: invalid start time");
}

#[test]
fn timezone_and_subsecond_times_are_unsigned_decimal_nanoseconds() {
    let mut body = common_body();
    body["startTime"] = json!("1970-01-01T01:00:00.000000001+01:00");
    body["endTime"] = json!("1970-01-01T00:00:00.123456789Z");
    let wire = wire(&[event("span-create", body)]);
    let span = &spans(&wire)[0];
    assert_eq!(span["startTimeUnixNano"], "1");
    assert_eq!(span["endTimeUnixNano"], "123456789");
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
