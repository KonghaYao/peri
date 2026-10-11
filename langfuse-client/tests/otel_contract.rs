use std::sync::{Arc, Mutex};

use langfuse_client::types::{session::SessionBody, TraceBody};
use langfuse_client::{GenerationBody, IngestionEvent, LangfuseClient, SpanBody};

fn valid_id(value: &serde_json::Value, length: usize) {
    let text = value.as_str().unwrap();
    assert_eq!(text.len(), length);
    assert!(text.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(text.bytes().any(|byte| byte != b'0'));
}

#[tokio::test]
async fn exported_http_canary_contains_only_complete_spans_with_valid_parent_context() {
    let mut server = mockito::Server::new_async().await;
    let captured = Arc::new(Mutex::new(None));
    let capture = Arc::clone(&captured);
    let response = server
        .mock("POST", "/api/public/otel/v1/traces")
        .match_header("authorization", "Basic cGs6c2s=")
        .match_header("x-langfuse-ingestion-version", "4")
        .with_status(200)
        .with_body_from_request(move |request| {
            *capture.lock().unwrap() =
                Some(serde_json::from_slice::<serde_json::Value>(request.body().unwrap()).unwrap());
            "{}".into()
        })
        .expect(1)
        .create_async()
        .await;
    let trace_id = "019a0000-0000-7000-8000-000000000001";
    let span_id = "span_019a0000-0000-7000-8000-000000000002";
    let timestamp = "2026-09-30T00:00:00Z";
    let events = vec![
        IngestionEvent::TraceCreate {
            id: "root-envelope".into(),
            timestamp: timestamp.into(),
            body: TraceBody {
                id: Some(trace_id.into()),
                name: Some("root".into()),
                session_id: Some("canary-session".into()),
                ..Default::default()
            },
            metadata: None,
        },
        IngestionEvent::SessionCreate {
            id: "session-envelope".into(),
            timestamp: timestamp.into(),
            body: SessionBody {
                id: "canary-session".into(),
                ..Default::default()
            },
            metadata: None,
        },
        IngestionEvent::SpanCreate {
            id: "span-envelope".into(),
            timestamp: timestamp.into(),
            body: SpanBody {
                id: Some(span_id.into()),
                trace_id: Some(trace_id.into()),
                parent_observation_id: Some(trace_id.into()),
                name: Some("child".into()),
                start_time: Some(timestamp.into()),
                end_time: Some("2026-09-30T00:00:01Z".into()),
                ..Default::default()
            },
            metadata: None,
        },
        IngestionEvent::GenerationCreate {
            id: "generation-envelope".into(),
            timestamp: timestamp.into(),
            body: GenerationBody {
                id: Some("gen_019a0000-0000-7000-8000-000000000003".into()),
                trace_id: Some(trace_id.into()),
                parent_observation_id: Some(span_id.into()),
                name: Some("generation".into()),
                start_time: Some(timestamp.into()),
                end_time: Some("2026-09-30T00:00:01Z".into()),
                input: Some(serde_json::json!({"messages": ["complete-input"]})),
                output: Some(serde_json::json!("complete-output")),
                ..Default::default()
            },
            metadata: None,
        },
    ];
    LangfuseClient::new("pk", "sk", &server.url(), 0)
        .ingest(events)
        .await
        .unwrap();
    response.assert_async().await;
    let request = captured.lock().unwrap().take().unwrap();
    let spans = request["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap();
    assert_eq!(spans.len(), 3);
    for span in spans {
        valid_id(&span["traceId"], 32);
        valid_id(&span["spanId"], 16);
        assert_eq!(span["traceId"], spans[0]["traceId"]);
        let start = span["startTimeUnixNano"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let end = span["endTimeUnixNano"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(end >= start);
    }
    assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
    assert_eq!(spans[2]["parentSpanId"], spans[1]["spanId"]);
    assert_ne!(spans[0]["spanId"], spans[0]["traceId"]);
}
