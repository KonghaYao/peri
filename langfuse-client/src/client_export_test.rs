//! 导出入口的契约测试（契约 §3.1、§3.4、§5、§8）：双编码、双限额与暂存封装。
//!
//! 覆盖 `gzip_and_none_share_otlp_semantics`（none/gzip 共用一份 OTLP 语义与元数据）、
//! `raw_and_wire_limits_are_independent`（两个上限各自独立生效）与
//! `otlp_expansion_reports_a_splittable_limit_error`（编码膨胀返回可区分信号，拆批归 P3a）；
//! 旧格式逐字节基线在 `encoding/stream_test.rs::frozen_baseline_bytes_are_stable`。
//!
//! 契约 §8 建议把 `gzip_and_none_share_otlp_semantics` 放在 `encoding/stream_test.rs`，但
//! `encode*` 入口在 `client` 内私有的 `mod export`，`crate::encoding` 的测试够不到它，故与
//! 其余编码入口用例一起留在这里。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use serde_json::{json, Value};

use super::*;
use crate::{
    client::LangfuseClient,
    config::TelemetryConfig,
    encoding::ExportContext,
    memory::MemoryBudget,
    payload::{ExportPayload, PayloadBody},
    snapshot::TelemetryEvent,
    spool::cleanup::CleanupRegistry,
    spool::{
        CleanupRegistration, ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor,
        QuotaReservation, RouteFingerprint, SpoolError, SpoolObject, SpoolPage, SpoolReader,
        SpoolResult, SpoolState, SpoolStore, SpoolUsage, TemporaryObject,
    },
    types::IngestionEvent,
};

const TRACE_ID: &str = "11111111111111111111111111111111";

#[test]
fn default_budgets_are_explicit() {
    let config = ExportConfig::default();
    assert_eq!(config.max_request_bytes, 4 * 1024 * 1024);
    assert_eq!(config.max_response_bytes, 64 * 1024);
    assert_eq!(config.retry_budget, Duration::from_secs(60));
    assert_eq!(config.compression, Compression::Gzip);
    assert_eq!(config.max_wire_bytes, 4 * 1024 * 1024);
}

#[test]
fn invalid_export_configurations_are_rejected() {
    let invalid = [
        ExportConfig {
            max_request_bytes: 0,
            ..Default::default()
        },
        ExportConfig {
            max_response_bytes: 0,
            ..Default::default()
        },
        ExportConfig {
            max_wire_bytes: 0,
            ..Default::default()
        },
        ExportConfig {
            retry_budget: Duration::ZERO,
            ..Default::default()
        },
        ExportConfig {
            retry_budget: Duration::MAX,
            ..Default::default()
        },
        ExportConfig {
            initial_retry_delay: Duration::ZERO,
            ..Default::default()
        },
        ExportConfig {
            max_retry_delay: Duration::from_millis(10),
            ..Default::default()
        },
    ];
    for config in invalid {
        assert!(matches!(
            LangfuseClient::new("pk", "sk", "http://unused", 1).with_export_config(config),
            Err(LangfuseError::Config(_))
        ));
    }
}

// ─── 受预算约束的 writer ────────────────────────────────────────────

fn budget(limit: usize) -> Arc<MemoryBudget> {
    MemoryBudget::new(limit).expect("budget")
}

/// `ExportPayload` 刻意不实现 `Debug`（正文不得因断言外泄），因此不能用 `unwrap_err`。
fn error_of<T>(result: Result<T, LangfuseError>) -> LangfuseError {
    match result {
        Ok(_) => panic!("必须失败"),
        Err(error) => error,
    }
}

fn event(value: Value) -> IngestionEvent {
    serde_json::from_value(value).expect("ingestion event")
}

/// 一小组事件：正文含转义与中文，覆盖多字节与转义膨胀。
fn events() -> Vec<IngestionEvent> {
    (0..8)
        .map(|index| {
            event(json!({
                "type": "event-create",
                "id": format!("event-{index}"),
                "timestamp": "1970-01-01T00:00:00Z",
                "body": {
                    "id": format!("{:016x}", index + 0x2000),
                    "traceId": TRACE_ID,
                    "name": format!("事件 \"{index}\""),
                    "startTime": "1970-01-01T00:00:00Z",
                    "input": {"文本": "中文😀".repeat(1 + index), "esc": "a\"b\\c\n\t"},
                    "output": ["x".repeat(200 + index * 50), {"n": index}],
                    "metadata": {"样本": index, "文本": "标签"},
                    "level": "WARNING",
                },
            }))
        })
        .collect()
}

fn telemetry(events: Vec<IngestionEvent>, budget: &Arc<MemoryBudget>) -> Vec<TelemetryEvent> {
    events
        .into_iter()
        .map(|event| TelemetryEvent::try_new(event, None, None, budget).expect("telemetry event"))
        .collect()
}

fn context() -> ExportContext {
    ExportContext {
        route: RouteFingerprint::from_bytes([9; 32]),
        accepted_at_ms: 0,
        expires_at_ms: 60_000,
    }
}

/// 构造受预算约束的 writer；`store` 为 `None` 时是纯内存路径。
fn writer(
    telemetry_config: &TelemetryConfig,
    budget: &Arc<MemoryBudget>,
    store: Option<Arc<dyn SpoolStore>>,
    wire: usize,
) -> BudgetedWriter {
    // 纯内存路径不会 `create`，清理槽只需要一个可构造的端口占位。
    let cleanup_store: Arc<dyn SpoolStore> = match store.clone() {
        Some(store) => store,
        None => MemStore::new(1 << 20),
    };
    let registry = CleanupRegistry::new(cleanup_store, Arc::clone(budget), 8);
    let registration = registry.try_register().expect("cleanup slot");
    BudgetedWriter::new(
        telemetry_config,
        Arc::clone(budget),
        store,
        context(),
        wire,
        registration,
    )
    .expect("budgeted writer")
}

fn payload_bytes(payload: &ExportPayload) -> Vec<u8> {
    match &payload.body {
        PayloadBody::Memory(bytes) => bytes.bytes().to_vec(),
        PayloadBody::File(object) => {
            let claim = object.take_claim().expect("claim");
            let mut reader = claim.open().expect("open");
            let mut out = Vec::new();
            reader.read_to_end(&mut out).expect("read");
            drop(reader);
            object.return_claim(claim).expect("return claim");
            out
        }
    }
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .expect("gunzip");
    out
}

#[test]
fn writer_stops_at_limit_and_never_grows_capacity_past_it() {
    let config = TelemetryConfig::default();
    let budget = budget(1024 * 1024);
    // 越限时 sink 记录精确分类（不回显正文），调用方据此区分两个限额。
    let mut sink = writer(&config, &budget, None, 4);
    assert!(sink.write_all(b"12345").is_err(), "一次性越限必须拒绝");
    assert_eq!(
        sink.take_failure().map(|failure| failure.describe()),
        Some("OTLP payload exceeds the 4-byte limit".to_string())
    );
    // 未取走失败原因时，`finish` 直接以同一分类失败，已写字节不再增长。
    let mut sink = writer(&config, &budget, None, 5);
    sink.write_all(b"123").expect("within limit");
    sink.write_all(b"45").expect("at limit");
    assert!(sink.write_all(b"6").is_err(), "越限写入必须失败");
    assert!(sink.write_all(b"7").is_err(), "越限后继续写入仍然失败");
    let error = error_of(sink.finish(5, Compression::None, 1));
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes: 5 }
    ));
}

#[test]
fn request_clones_share_the_once_serialized_body() {
    let config = ExportConfig {
        compression: Compression::None,
        ..Default::default()
    };
    let budget = budget(8 * 1024 * 1024);
    let events = events();
    let telemetry = telemetry(events, &budget);
    let telemetry_config = TelemetryConfig::default();
    let payload = encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, 1 << 20),
        &config,
    )
    .expect("encode");
    let bytes = match &payload.body {
        PayloadBody::Memory(bytes) => bytes.bytes().clone(),
        PayloadBody::File(_) => panic!("小正文必须驻留内存"),
    };
    assert_eq!(payload.wire_len, bytes.len() as u64, "只编码一次");
    let request = reqwest::Client::new()
        .post("http://unused")
        .body(bytes)
        .build()
        .unwrap();
    for _ in 0..3 {
        let cloned = request.try_clone().unwrap();
        assert_eq!(
            cloned.body().unwrap().as_bytes().unwrap().as_ptr(),
            request.body().unwrap().as_bytes().unwrap().as_ptr()
        );
    }
    assert_eq!(payload.event_count, 8);
}

struct InvalidPayload;

impl Serialize for InvalidPayload {
    fn serialize<Serializer: serde::Serializer>(
        &self,
        _: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error> {
        Err(serde::ser::Error::custom("secret-payload"))
    }
}

#[test]
fn serialization_error_does_not_expose_payload() {
    let error = preflight(&InvalidPayload, 32).unwrap_err();
    assert!(error.to_string().contains("preflight serialization failed"));
    assert!(!error.to_string().contains("secret-payload"));
    // 编码侧：越限与未就绪来源的错误文本都不得携带正文。
    let budget = budget(1024 * 1024);
    let secret = event(json!({
        "type": "trace-create", "id": "event", "timestamp": "1970-01-01T00:00:00Z",
        "body": {"id": TRACE_ID, "input": "SECRET-MARKER",
            "timestamp": "1970-01-01T00:00:00Z"},
    }));
    let telemetry = telemetry(vec![secret], &budget);
    let config = ExportConfig {
        compression: Compression::None,
        ..Default::default()
    };
    let telemetry_config = TelemetryConfig::default();
    let error = error_of(encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, 1),
        &config,
    ));
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes: 1 }
    ));
    assert!(!error.to_string().contains("SECRET-MARKER"));
}

#[test]
fn counting_writer_is_exact_bounded_and_overflow_safe() {
    let mut writer = CountingWriter {
        count: 0,
        limit: 5,
        exceeded: false,
    };
    writer.write_all(b"12345").unwrap();
    assert_eq!(writer.count, 5);
    assert!(writer.write_all(b"6").is_err());
    assert_eq!(writer.count, 5);
    assert!(writer.exceeded);
    assert!(preflight(&"123", 5).is_ok());
    assert!(matches!(
        preflight(&"1234", 5),
        Err(LangfuseError::PayloadTooLarge { limit_bytes: 5 })
    ));
    let mut writer = CountingWriter {
        count: usize::MAX - 1,
        limit: usize::MAX,
        exceeded: false,
    };
    assert!(writer.write_all(b"12").is_err());
    assert_eq!(writer.count, usize::MAX - 1);
    writer.write_all(b"1").unwrap();
    assert_eq!(writer.count, usize::MAX);
    assert!(writer.write_all(b"2").is_err());
}

#[test]
fn preflight_counts_json_escaping_and_total_array_envelope() {
    let payload = vec!["\"\\\n", "文本"];
    let expected = serde_json::to_vec(&payload).unwrap().len();
    preflight(&payload, expected).unwrap();
    assert!(
        matches!(preflight(&payload, expected - 1), Err(LangfuseError::PayloadTooLarge { limit_bytes }) if limit_bytes == expected - 1)
    );
}

// ─── 双编码与双限额 ────────────────────────────────────────────────

#[test]
fn gzip_and_none_share_otlp_semantics() {
    let budget = budget(8 * 1024 * 1024);
    let events = events();
    let telemetry = telemetry(events, &budget);
    let telemetry_config = TelemetryConfig::default();
    let none_config = ExportConfig {
        compression: Compression::None,
        ..Default::default()
    };
    let none = encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, 1 << 20),
        &none_config,
    )
    .expect("none encode");
    let none_bytes = payload_bytes(&none);
    assert_eq!(none.encoding, Compression::None);
    assert_eq!(
        none.raw_len,
        none_bytes.len() as u64,
        "none 的解压后字节即线包"
    );
    assert_eq!(none.wire_len, none_bytes.len() as u64);
    assert_eq!(none.event_count, 8);
    assert!(!none.is_memory() || none.body_len() == none.wire_len);

    let gzip_config = ExportConfig {
        compression: Compression::Gzip,
        ..Default::default()
    };
    let gzip = encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, 1 << 20),
        &gzip_config,
    )
    .expect("gzip encode");
    let gzip_bytes = payload_bytes(&gzip);
    assert_eq!(gzip.encoding, Compression::Gzip);
    assert_eq!(
        gzip.raw_len,
        none_bytes.len() as u64,
        "解压后必须是同一份 OTLP"
    );
    assert_eq!(
        gzip.wire_len,
        gzip_bytes.len() as u64,
        "trailer 计入线包长度"
    );
    assert_eq!(gzip.event_count, 8);
    assert_eq!(gunzip(&gzip_bytes), none_bytes, "gzip 解压后等同 none");
    assert_ne!(gzip_bytes, none_bytes, "gzip 必须真的压缩");
    assert!(gzip_bytes.starts_with(&[0x1f, 0x8b]), "标准 gzip 头");
    assert_eq!(gzip.sha256, sha2_of(&gzip_bytes));
    assert_eq!(none.sha256, sha2_of(&none_bytes));
}

fn sha2_of(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

#[test]
fn raw_and_wire_limits_are_independent() {
    let budget = budget(8 * 1024 * 1024);
    let events = events();
    let telemetry = telemetry(events, &budget);
    let telemetry_config = TelemetryConfig::default();
    let raw_len = crate::encoding::write_otlp(&telemetry, &mut Vec::new()).expect("measure raw");
    assert!(raw_len as usize > 2_048, "样本要足够大才能区分两个上限");

    // 只超解压后上限：线包上限远大于解压后正文。
    let raw_only = ExportConfig {
        max_request_bytes: 2_048,
        max_wire_bytes: 1 << 20,
        compression: Compression::Gzip,
        ..Default::default()
    };
    let error = error_of(encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, raw_only.max_wire_bytes),
        &raw_only,
    ));
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes: 2_048 }
    ));

    // 只超线包上限：解压后上限远大于正文，线包上限取实测压缩结果的一半。
    let compressed = payload_bytes(
        &encode(
            &telemetry,
            writer(&telemetry_config, &budget, None, 1 << 20),
            &ExportConfig {
                compression: Compression::Gzip,
                ..Default::default()
            },
        )
        .expect("baseline gzip"),
    );
    assert!(
        (compressed.len() as u64) < raw_len,
        "样本必须真的压缩才能区分两个上限：{} vs {raw_len}",
        compressed.len()
    );
    let wire_only = ExportConfig {
        max_request_bytes: 1 << 20,
        max_wire_bytes: compressed.len() / 2,
        compression: Compression::Gzip,
        ..Default::default()
    };
    let error = error_of(encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, wire_only.max_wire_bytes),
        &wire_only,
    ));
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == wire_only.max_wire_bytes
    ));

    // none 模式下线包上限更小：仍按线包上限精确报错。
    let none_wire = ExportConfig {
        max_request_bytes: 1 << 20,
        max_wire_bytes: 1_024,
        compression: Compression::None,
        ..Default::default()
    };
    let error = error_of(encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, none_wire.max_wire_bytes),
        &none_wire,
    ));
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes: 1_024 }
    ));
}

#[test]
fn otlp_expansion_reports_a_splittable_limit_error() {
    let budget = budget(8 * 1024 * 1024);
    // 事件原文很小，但每个 `"` 在 OTLP 里翻倍：编码后正文远超原文。
    let text = "\"".repeat(512);
    let event = event(json!({
        "type": "trace-create", "id": "event", "timestamp": "1970-01-01T00:00:00Z",
        "body": {"id": TRACE_ID, "input": text, "timestamp": "1970-01-01T00:00:00Z"},
    }));
    let raw = serde_json::to_vec(&event).expect("event bytes").len();
    let telemetry = telemetry(vec![event], &budget);
    let encoded = crate::encoding::write_otlp(&telemetry, &mut Vec::new()).expect("encoded");
    assert!(encoded as usize > raw, "OTLP 编码必须膨胀");
    let limit = (raw + encoded as usize) / 2;
    let error = crate::encoding::measure_otlp(&telemetry, limit).unwrap_err();
    assert!(
        matches!(error, LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == limit),
        "必须给出可区分于转换/暂存错误的超限信号，供 P3a 拆批"
    );
    let config = ExportConfig {
        compression: Compression::None,
        ..Default::default()
    };
    let telemetry_config = TelemetryConfig::default();
    let error = error_of(encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, limit),
        &config,
    ));
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == limit
    ));
}

#[test]
fn export_event_count_excludes_records_without_spans() {
    // sdk-log 与会话记录不产生 span，不得计入 event_count（契约 §5.2）。
    let budget = budget(8 * 1024 * 1024);
    let telemetry = telemetry(
        vec![
            event(json!({
                "type": "trace-create", "id": "trace-1", "timestamp": "1970-01-01T00:00:00Z",
                "body": {"id": TRACE_ID, "timestamp": "1970-01-01T00:00:00Z"},
            })),
            event(json!({
                "type": "span-create", "id": "span-1", "timestamp": "1970-01-01T00:00:01Z",
                "body": {"id": "2222222222222222", "traceId": TRACE_ID, "name": "span",
                    "startTime": "1970-01-01T00:00:01Z"},
            })),
            event(json!({
                "type": "sdk-log", "id": "log-1", "timestamp": "1970-01-01T00:00:02Z",
                "body": {"log": {"m": "sdk"}},
            })),
            event(json!({
                "type": "session-create", "id": "sess-1", "timestamp": "1970-01-01T00:00:03Z",
                "body": {"id": "s-1"},
            })),
        ],
        &budget,
    );
    for compression in [Compression::None, Compression::Gzip] {
        let payload = encode(
            &telemetry,
            writer(&TelemetryConfig::default(), &budget, None, 1 << 20),
            &ExportConfig {
                compression,
                ..Default::default()
            },
        )
        .expect("encode");
        let body = payload_bytes(&payload);
        let body = if compression == Compression::Gzip {
            gunzip(&body)
        } else {
            body
        };
        let spans: Value = serde_json::from_slice(&body).expect("OTLP body is JSON");
        assert_eq!(
            spans["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .expect("spans array")
                .len(),
            2
        );
        assert_eq!(payload.event_count, 2, "event_count 只数写出的 span");
    }
}

// ─── 暂存路径 ───────────────────────────────────────────────────────

#[test]
fn spooled_export_body_matches_memory_body() {
    let budget = budget(8 * 1024 * 1024);
    let events = events();
    let telemetry = telemetry(events, &budget);
    let store: Arc<dyn SpoolStore> = MemStore::new(1 << 20);
    let telemetry_config = TelemetryConfig {
        payload_memory_threshold_bytes: 256,
        ..Default::default()
    };
    for compression in [Compression::None, Compression::Gzip] {
        let config = ExportConfig {
            compression,
            ..Default::default()
        };
        let in_memory = encode(
            &telemetry,
            writer(&telemetry_config, &budget, None, 1 << 20),
            &config,
        )
        .expect("memory encode");
        let spooled = encode(
            &telemetry,
            writer(
                &telemetry_config,
                &budget,
                Some(Arc::clone(&store)),
                1 << 20,
            ),
            &config,
        )
        .expect("spooled encode");
        assert!(in_memory.is_memory(), "阈值以下必须驻留内存");
        assert!(!spooled.is_memory(), "跨阈值必须转盘");
        let memory_bytes = payload_bytes(&in_memory);
        let spooled_bytes = payload_bytes(&spooled);
        assert_eq!(spooled_bytes, memory_bytes, "两条路径的正文必须逐字节相同");
        assert_eq!(spooled.raw_len, in_memory.raw_len);
        assert_eq!(spooled.wire_len, spooled_bytes.len() as u64);
        assert_eq!(spooled.sha256, sha2_of(&spooled_bytes));
        assert_eq!(spooled.event_count, 8);
        match &spooled.body {
            PayloadBody::File(object) => {
                assert_eq!(object.meta().stored_len, spooled.wire_len);
                assert_eq!(object.meta().raw_len, spooled.raw_len);
                assert_eq!(object.meta().encoding, compression);
                assert_eq!(object.meta().state, SpoolState::Ready);
                assert_eq!(object.meta().sha256, spooled.sha256);
                assert_eq!(object.meta().expires_at_ms, context().expires_at_ms);
            }
            PayloadBody::Memory(_) => panic!("跨阈值必须是文件正文"),
        }
    }
}

#[test]
fn finish_hands_the_prefix_permit_to_the_payload() {
    let budget = budget(1 << 20);
    let telemetry_config = TelemetryConfig::default();
    let mut sink = writer(&telemetry_config, &budget, None, 16);
    let reserved = budget.used();
    assert_eq!(reserved, 16, "writer 按 wire 上限预约内存前缀额度");
    sink.write_all(b"1234").expect("write");
    let payload = sink.finish(4, Compression::None, 1).expect("finish");
    assert_eq!(payload.raw_len, 4);
    assert_eq!(payload.wire_len, 4);
    assert!(payload.is_memory());
    assert_eq!(payload.body_len(), 4);
    assert_eq!(
        budget.used(),
        reserved,
        "finish 后 permit 仍被 payload 持有"
    );
    drop(payload);
    assert_eq!(budget.used(), 0, "payload 释放后额度才归还");
}

// ─── HTTP 侧的既有回归 ─────────────────────────────────────────────

#[tokio::test]
async fn direct_ingest_oversized_input_is_rejected_before_conversion_or_http() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let limit = ExportConfig::default().max_request_bytes;
    for root in [Some("root".into()), None] {
        let event = IngestionEvent::TraceCreate {
            id: "event".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            body: crate::types::TraceBody {
                id: root,
                input: Some(serde_json::Value::String("x".repeat(limit + 1))),
                ..Default::default()
            },
            metadata: None,
        };
        let error = LangfuseClient::new("pk", "sk", &server.url(), 3)
            .ingest(vec![event])
            .await
            .unwrap_err();
        assert!(
            matches!(error, LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == limit)
        );
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn worker_and_client_request_limits_use_the_minimum_before_sending() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .expect(0)
        .create_async()
        .await;
    for (client_limit, worker_limit, expected) in [(1, 1024, 1), (1024, 1, 1), (1024, 0, 0)] {
        let client = LangfuseClient::new("pk", "sk", &server.url(), 3)
            .with_export_config(ExportConfig {
                max_request_bytes: client_limit,
                ..Default::default()
            })
            .unwrap();
        let event = IngestionEvent::TraceCreate {
            id: "event".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            body: crate::types::TraceBody {
                id: Some("root".into()),
                ..Default::default()
            },
            metadata: None,
        };
        assert!(
            matches!(client.ingest_with_limit(vec![event], worker_limit).await, Err(LangfuseError::PayloadTooLarge { limit_bytes }) if limit_bytes == expected)
        );
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn encoded_request_exactly_at_limit_is_accepted() {
    let event = IngestionEvent::TraceCreate {
        id: "event".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: crate::types::TraceBody {
            id: Some("root".into()),
            ..Default::default()
        },
        metadata: None,
    };
    let budget = budget(8 * 1024 * 1024);
    let telemetry = telemetry(vec![event.clone()], &budget);
    let telemetry_config = TelemetryConfig::default();
    let config = ExportConfig {
        compression: Compression::None,
        ..Default::default()
    };
    let payload = encode(
        &telemetry,
        writer(&telemetry_config, &budget, None, 1 << 20),
        &config,
    )
    .expect("encode");
    let encoded = payload_bytes(&payload);
    let limit = encoded.len();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .match_body(encoded.clone())
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let client = LangfuseClient::new("pk", "sk", &server.url(), 0)
        .with_export_config(ExportConfig {
            max_request_bytes: limit,
            compression: Compression::None,
            ..Default::default()
        })
        .unwrap();
    client
        .ingest_with_limit(vec![event], usize::MAX)
        .await
        .unwrap();
    mock.assert_async().await;
}

// ─── 最小暂存端口（测试局部 fixture） ──────────────────────────────

/// 进程内暂存端口：`create` 写入内存缓冲，`publish` 后可按 id 领取。
struct MemStore {
    threshold: usize,
    next: AtomicU64,
    self_ref: std::sync::OnceLock<std::sync::Weak<MemStore>>,
    objects: Mutex<HashMap<ObjectId, Bytes>>,
    in_flight: Mutex<HashMap<ObjectId, Arc<Mutex<Vec<u8>>>>>,
}

impl crate::spool::sealed::Sealed for MemStore {}

impl MemStore {
    fn new(threshold: usize) -> Arc<Self> {
        let store = Arc::new(Self {
            threshold,
            next: AtomicU64::new(1),
            self_ref: std::sync::OnceLock::new(),
            objects: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
        });
        let _ = store.self_ref.set(Arc::downgrade(&store));
        store
    }

    fn handle(&self) -> Arc<dyn SpoolStore> {
        let store = self
            .self_ref
            .get()
            .expect("self handle")
            .upgrade()
            .expect("store alive");
        let handle: Arc<dyn SpoolStore> = store;
        handle
    }

    fn allocate(&self) -> ObjectId {
        let counter = self.next.fetch_add(1, Ordering::Relaxed);
        let mut raw = [0u8; 16];
        raw[..8].copy_from_slice(&counter.to_be_bytes());
        ObjectId::from_bytes(raw)
    }

    fn bytes(&self, id: ObjectId) -> SpoolResult<Bytes> {
        self.objects
            .lock()
            .expect("objects")
            .get(&id)
            .cloned()
            .ok_or(SpoolError::NotFound)
    }

    fn meta(&self, id: ObjectId) -> ObjectMeta {
        let len = self
            .objects
            .lock()
            .expect("objects")
            .get(&id)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or_default();
        ObjectMeta {
            version: 1,
            id,
            kind: ObjectKind::Export,
            route: RouteFingerprint::from_bytes([9; 32]),
            event_count: 0,
            encoding: crate::config::Compression::None,
            raw_len: len,
            stored_len: len,
            sha256: [0; 32],
            accepted_at_ms: 0,
            expires_at_ms: 60_000,
            attempts: 0,
            next_attempt_at_ms: None,
            state: SpoolState::Ready,
        }
    }
}

impl SpoolStore for MemStore {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>> {
        struct Reserved(u64);
        impl QuotaReservation for Reserved {
            fn bytes(&self) -> u64 {
                self.0
            }
        }
        Ok(Box::new(Reserved(bytes)))
    }

    fn create(
        &self,
        quota: Box<dyn QuotaReservation>,
        _kind: ObjectKind,
        cleanup: CleanupRegistration,
    ) -> SpoolResult<Box<dyn TemporaryObject>> {
        let id = self.allocate();
        cleanup.slot.bind(id, quota.bytes())?;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        self.in_flight
            .lock()
            .expect("in flight")
            .insert(id, Arc::clone(&buffer));
        Ok(Box::new(MemObject {
            id,
            handle: self.handle(),
            threshold: self.threshold,
            buffer,
            cleanup: Some(cleanup),
        }))
    }

    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta> {
        self.bytes(id).map(|_| self.meta(id))
    }

    fn open(&self, id: ObjectId) -> SpoolResult<SpoolReader> {
        Ok(Box::new(std::io::Cursor::new(self.bytes(id)?)))
    }

    fn list(&self, _after: Option<PageCursor>, _limit: usize) -> SpoolResult<SpoolPage> {
        Err(SpoolError::Unavailable)
    }

    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        let mut held = self.in_flight.lock().expect("in flight");
        if let Some(buffer) = held.remove(&id) {
            let bytes = Bytes::from(buffer.lock().expect("buffer").clone());
            return Ok(Some(Box::new(MemClaim {
                meta: self.meta(id),
                bytes,
            })));
        }
        Ok(Some(Box::new(MemClaim {
            meta: self.meta(id),
            bytes: self.bytes(id)?,
        })))
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        self.objects
            .lock()
            .expect("objects")
            .remove(&id)
            .map(|_| ())
            .ok_or(SpoolError::NotFound)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        let objects = self.objects.lock().expect("objects");
        let stored_bytes = objects.values().map(|bytes| bytes.len() as u64).sum();
        Ok(SpoolUsage {
            reserved_bytes: stored_bytes,
            stored_bytes,
            objects: objects.len() as u64,
        })
    }
}

struct MemObject {
    id: ObjectId,
    handle: Arc<dyn SpoolStore>,
    threshold: usize,
    buffer: Arc<Mutex<Vec<u8>>>,
    cleanup: Option<CleanupRegistration>,
}

impl Write for MemObject {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len()
            > self
                .threshold
                .saturating_sub(self.buffer.lock().expect("buffer").len())
        {
            return Err(std::io::Error::other("fake spool quota exceeded"));
        }
        self.buffer.lock().expect("buffer").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl TemporaryObject for MemObject {
    fn id(&self) -> ObjectId {
        self.id
    }

    fn written_len(&self) -> u64 {
        self.buffer.lock().expect("buffer").len() as u64
    }

    fn publish(mut self: Box<Self>, meta: ObjectMeta) -> SpoolResult<SpoolObject> {
        let bytes = Bytes::from(std::mem::take(&mut *self.buffer.lock().expect("buffer")));
        let claim: Box<dyn ObjectClaim> = Box::new(MemClaim {
            meta: meta.clone(),
            bytes,
        });
        let cleanup = self.cleanup.take().ok_or(SpoolError::Unavailable)?;
        SpoolObject::from_claim(self.handle.clone(), claim, cleanup)
    }

    fn abort(self: Box<Self>) -> SpoolResult<()> {
        Ok(())
    }
}

struct MemClaim {
    meta: ObjectMeta,
    bytes: Bytes,
}

impl ObjectClaim for MemClaim {
    fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    fn open(&self) -> SpoolResult<SpoolReader> {
        Ok(Box::new(std::io::Cursor::new(self.bytes.clone())))
    }

    fn checkpoint(&mut self, _meta: ObjectMeta) -> SpoolResult<()> {
        Ok(())
    }

    fn delete(&mut self) -> SpoolResult<()> {
        Ok(())
    }
}
