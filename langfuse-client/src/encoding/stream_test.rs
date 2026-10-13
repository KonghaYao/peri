//! 统一流式编码的契约测试（契约 §3.4、§5、§8）：内容保真、跨块转义与计量口径。
//!
//! 覆盖：
//! - `hundred_boundary_inputs_roundtrip_exact_bytes`：128 个固定种子边界样本，逐字节比对
//!   「冻结 JSON 基准序列化字节」与「解出 OTLP `stringValue` 字节」，inline 与暂存对象各半；
//! - `escaped_utf8_across_chunk_boundary`：读写块 1..64 KiB 下逐字节等价，多字节字符跨块不切断；
//! - `frozen_baseline_bytes_are_stable` 在 `client_export_test.rs`（那里才有 `encode*` 入口）。
//!
//! `gzip`/`none`、双限额与编码膨胀用例同样需要 `client_export` 的入口（`mod export` 在 `client`
//! 内私有），放在 `client_export_test.rs`；本文件只驱动 `crate::encoding` 自己的端口。

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use serde_json::{json, Value};

use super::{measure_otlp, write_otlp};
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::{BudgetedBytes, InputSnapshot};
use crate::snapshot::{InputSource, TelemetryEvent};
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor, QuotaReservation, RouteFingerprint,
    SpoolError, SpoolObject, SpoolPage, SpoolReader, SpoolResult, SpoolState, SpoolStore,
    SpoolUsage,
};
use crate::stable_json::StableJson;
use crate::types::IngestionEvent;

// ─── 固定种子样本 ───────────────────────────────────────────────────

/// 线性同余发生器：固定种子保证每次运行生成同一批样本。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// 边界片段池：中文、emoji、JSON 转义字符、控制字符、空值、长文本、DEL。
const FRAGMENTS: &[&str] = &[
    "",
    "中",
    "中文😀",
    "a\"b\\c",
    "\n\t\r\u{8}\u{c}",
    "\u{0}\u{1}\u{1f}",
    "标签#引号\"反斜杠\\",
    "组合é́̂",
    "0123456789",
    "x",
    "```json\n{\"k\":\"v\"}\n```",
    "\u{7f}DEL",
];

fn random_text(rng: &mut Rng) -> String {
    let mut text = String::new();
    for _ in 0..1 + rng.below(4) {
        text.push_str(FRAGMENTS[rng.below(FRAGMENTS.len())]);
    }
    if rng.below(16) == 0 {
        text.push_str(&"大".repeat(2_000));
    }
    text
}

fn random_value(rng: &mut Rng, depth: usize) -> Value {
    match rng.below(if depth >= 3 { 6 } else { 9 }) {
        0 => json!(null),
        1 => json!(rng.below(2) == 0),
        2 => json!(rng.next() as i64 - 9_000_000_000),
        3 => json!((rng.next() % 100_000) as f64 / 8.0),
        4 => json!(random_text(rng)),
        5 => json!([]),
        6 => Value::Array(
            (0..rng.below(4))
                .map(|_| random_value(rng, depth + 1))
                .collect(),
        ),
        7 => json!({}),
        _ => {
            let mut map = serde_json::Map::new();
            for index in 0..rng.below(4) {
                map.insert(
                    format!("{}{index}", random_text(rng)),
                    random_value(rng, depth + 1),
                );
            }
            Value::Object(map)
        }
    }
}

/// 128 个样本：先放显式边界用例，再用固定种子补足。
fn sample_values() -> Vec<Value> {
    let mut values = vec![
        json!([]),
        json!({}),
        json!(""),
        json!(null),
        json!(true),
        json!(0),
        json!(-9_007_199_254_740_991i64),
        json!(1.7976931348623157e308),
        json!("中文😀\"\\\n\t\r\u{1}\u{1f}\u{0}"),
        json!({"空":"","零":0,"假":false,"空值":null}),
        json!([[[[["深"]]]]]),
        json!({"大":"大".repeat(4_000)}),
        json!({"a\"b":"c\\d"}),
        json!({"emoji":["😀","🎉"],"nested":{"deep":[1,2,{"x":null}]}}),
    ];
    let mut rng = Rng::new(0x5eed_1234_abcd_0001);
    while values.len() < 128 {
        values.push(random_value(&mut rng, 0));
    }
    values
}

// ─── 冻结来源与最小暂存端口 ─────────────────────────────────────────

const TRACE_ID: &str = "11111111111111111111111111111111";
const PARENT_ID: &str = "3333333333333333";

fn budget(limit: usize) -> Arc<MemoryBudget> {
    MemoryBudget::new(limit).expect("budget")
}

/// 冻结字节的内存来源（低于阈值时的 inline 路径）。
fn inline_source(budget: &Arc<MemoryBudget>, bytes: &[u8]) -> InputSource {
    let permit = budget.try_reserve(bytes.len()).expect("permit");
    InputSource::Ready(InputSnapshot::Inline(
        BudgetedBytes::new(Bytes::copy_from_slice(bytes), permit).expect("bytes"),
    ))
}

/// 冻结字节的暂存来源（超过阈值时的 file 路径）。
fn spooled_source(store: &Arc<MemStore>, budget: &Arc<MemoryBudget>, bytes: &[u8]) -> InputSource {
    InputSource::Ready(InputSnapshot::Spooled(publish(store, budget, bytes)))
}

fn publish(store: &Arc<MemStore>, budget: &Arc<MemoryBudget>, bytes: &[u8]) -> SpoolObject {
    let registry = CleanupRegistry::new(dyn_store(store), Arc::clone(budget), 8);
    let registration = registry.try_register().expect("slot");
    let id = store.put(bytes);
    registration
        .slot
        .bind(id, bytes.len() as u64)
        .expect("bind");
    let claim = store.claim(id).expect("claim call").expect("available");
    SpoolObject::from_claim(dyn_store(store), claim, registration).expect("from_claim")
}

/// 测试用最小暂存端口：字节留在进程内，只走端口语义（reserve/claim/open/return）。
struct MemStore {
    next: AtomicU64,
    objects: Mutex<HashMap<ObjectId, Bytes>>,
}

impl crate::spool::sealed::Sealed for MemStore {}

impl MemStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next: AtomicU64::new(1),
            objects: Mutex::new(HashMap::new()),
        })
    }

    fn put(&self, bytes: &[u8]) -> ObjectId {
        let counter = self.next.fetch_add(1, Ordering::Relaxed);
        let mut raw = [0u8; 16];
        raw[..8].copy_from_slice(&counter.to_be_bytes());
        let id = ObjectId::from_bytes(raw);
        self.objects
            .lock()
            .expect("objects")
            .insert(id, Bytes::copy_from_slice(bytes));
        id
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
        let len = self.bytes(id).map(|bytes| bytes.len()).unwrap_or_default() as u64;
        ObjectMeta {
            version: 1,
            id,
            kind: ObjectKind::Input,
            route: RouteFingerprint::from_bytes([7; 32]),
            event_count: 1,
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

/// 显式 unsize：返回值位置的期望类型会把 `Arc::clone` 的泛型推到 `dyn SpoolStore`。
fn dyn_store(store: &Arc<MemStore>) -> Arc<dyn SpoolStore> {
    let handle: Arc<MemStore> = Arc::clone(store);
    handle
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
        _quota: Box<dyn QuotaReservation>,
        _kind: ObjectKind,
        _cleanup: crate::spool::CleanupRegistration,
    ) -> SpoolResult<Box<dyn crate::spool::TemporaryObject>> {
        Err(SpoolError::Unavailable)
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

/// 事件 fixture：与 `conversion_test.rs` 同口径，按 serde 表示构造事件。
fn event(value: Value) -> IngestionEvent {
    serde_json::from_value(value).expect("ingestion event")
}

// ─── 事件样本 ───────────────────────────────────────────────────────

/// 一个样本事件：原文（body 的 input/output 为空）+ 冻结 source + 期望属性键与基准字节。
struct Sample {
    event: IngestionEvent,
    input: Option<InputSource>,
    output: Option<InputSource>,
    input_key: &'static str,
    output_key: &'static str,
    baseline_input: Vec<u8>,
    baseline_output: Vec<u8>,
    metadata: Value,
    metadata_namespace: &'static str,
}

/// 五种 span 家族轮流取样：input/output/metadata/ID/时间都参与保真比对。
fn sample(
    index: usize,
    values: &[Value],
    budget: &Arc<MemoryBudget>,
    store: &Arc<MemStore>,
    spooled: bool,
) -> Sample {
    let id = format!("{:016x}", index as u64 + 0x1000);
    let input = &values[index % values.len()];
    let output = &values[(index * 7 + 3) % values.len()];
    let metadata = json!({"样本": index, "文本": random_text(&mut Rng::new(index as u64))});
    let baseline_input = serde_json::to_vec(input).expect("baseline input");
    let baseline_output = serde_json::to_vec(output).expect("baseline output");
    let source = |value: &Value| {
        let bytes = serde_json::to_vec(value).expect("frozen bytes");
        if spooled {
            spooled_source(store, budget, &bytes)
        } else {
            inline_source(budget, &bytes)
        }
    };
    let common = json!({
        "id": id,
        "traceId": TRACE_ID,
        "parentObservationId": PARENT_ID,
        "startTime": "1970-01-01T00:00:10Z",
        "metadata": metadata.clone(),
        "version": "v1",
        "environment": "dev",
    });
    let (event, input_key, output_key, metadata_namespace) = match index % 5 {
        0 => (
            event(json!({
                "type": "event-create", "id": id, "timestamp": "1970-01-01T00:00:10Z",
                "body": merge(&common, json!({"name": "事件", "level": "WARNING",
                    "statusMessage": "warn \"msg\""})),
            })),
            "langfuse.observation.input",
            "langfuse.observation.output",
            "langfuse.observation.metadata",
        ),
        1 => (
            event(json!({
                "type": "span-create", "id": id, "timestamp": "1970-01-01T00:00:11Z",
                "body": merge(&common, json!({"name": "span", "level": "ERROR",
                    "statusMessage": "失败", "endTime": "1970-01-01T00:00:12Z"})),
            })),
            "langfuse.observation.input",
            "langfuse.observation.output",
            "langfuse.observation.metadata",
        ),
        2 => (
            event(json!({
                "type": "generation-create", "id": id, "timestamp": "1970-01-01T00:00:13Z",
                "body": merge(&common, json!({"name": "generation", "level": "DEBUG",
                    "endTime": "1970-01-01T00:00:14Z", "model": "gpt-4o",
                    "completionStartTime": "1970-01-01T00:00:13.500Z", "promptName": "p1"})),
            })),
            "langfuse.observation.input",
            "langfuse.observation.output",
            "langfuse.observation.metadata",
        ),
        3 => (
            event(json!({
                "type": "observation-create", "id": id, "timestamp": "1970-01-01T00:00:15Z",
                "body": merge(&common, json!({"type": "TOOL", "model": "模型"})),
            })),
            "langfuse.observation.input",
            "langfuse.observation.output",
            "langfuse.observation.metadata",
        ),
        _ => (
            event(json!({
                "type": "trace-create", "id": id, "timestamp": "1970-01-01T00:00:16Z",
                "body": {"id": id, "name": "trace \"名\"", "userId": "用户",
                    "metadata": metadata.clone(), "tags": ["标签", "b"],
                    "timestamp": "1970-01-01T00:00:16Z"},
            })),
            "langfuse.trace.input",
            "langfuse.trace.output",
            "langfuse.trace.metadata",
        ),
    };
    Sample {
        event,
        input: Some(source(input)),
        output: Some(source(output)),
        input_key,
        output_key,
        baseline_input,
        baseline_output,
        metadata,
        metadata_namespace,
    }
}

/// 浅合并两组对象键（后者覆盖前者），用于把公共字段拼进各 body。
fn merge(base: &Value, extra: Value) -> Value {
    let mut merged = base.as_object().expect("object").clone();
    for (key, value) in extra.as_object().expect("object") {
        merged.insert(key.clone(), value.clone());
    }
    Value::Object(merged)
}

// ─── OTLP 解码断言 ─────────────────────────────────────────────────

fn spans(body: &[u8]) -> Vec<Value> {
    let wire: Value = serde_json::from_slice(body).expect("OTLP body is JSON");
    wire["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .expect("spans array")
        .clone()
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|attr| attr["key"].as_str() == Some(key))
        .map(|attr| &attr["value"])
}

/// 属性文本必须与冻结字节逐字节相同（保真的唯一判据）。
fn assert_exact(span: &Value, key: &str, baseline: &[u8]) {
    let text = attr(span, key)
        .and_then(|value| value.get("stringValue"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string attribute {key}"));
    assert_eq!(
        text.as_bytes(),
        baseline,
        "attribute {key} must match bytes"
    );
}

fn write_one(event: &TelemetryEvent) -> Vec<u8> {
    let mut body = Vec::new();
    let written = write_otlp(std::slice::from_ref(event), &mut body).expect("write otlp");
    assert_eq!(written, body.len() as u64, "返回长度必须等于实际字节数");
    body
}

// ─── 用例 ──────────────────────────────────────────────────────────

#[test]
fn hundred_boundary_inputs_roundtrip_exact_bytes() {
    let values = sample_values();
    assert!(values.len() >= 100, "至少 100 个固定种子样本");
    let budget = budget(64 * 1024 * 1024);
    let store = MemStore::new();
    let (mut inline_samples, mut spooled_samples) = (0usize, 0usize);
    for index in 0..values.len() {
        let spooled = index % 2 == 1;
        let sample = sample(index, &values, &budget, &store, spooled);
        let event = TelemetryEvent::try_new(sample.event, sample.input, sample.output, &budget)
            .expect("telemetry event");
        let body = write_one(&event);
        if spooled {
            spooled_samples += 1;
        } else {
            inline_samples += 1;
        }
        let spans = spans(&body);
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert_exact(span, sample.input_key, &sample.baseline_input);
        assert_exact(span, sample.output_key, &sample.baseline_output);
        // metadata 走 stable_json 口径（字典序），期望同样按该口径生成，不依赖构建期 feature。
        assert_exact(
            span,
            sample.metadata_namespace,
            &serde_json::to_vec(&StableJson(&sample.metadata)).expect("metadata bytes"),
        );
        let fields = sample.metadata.as_object().expect("object");
        let mut keys: Vec<&String> = fields.keys().collect();
        keys.sort_unstable();
        for key in keys {
            let value = &fields[key];
            let expected = match value.as_str() {
                Some(text) => text.as_bytes().to_vec(),
                None => serde_json::to_vec(&StableJson(value)).expect("field bytes"),
            };
            assert_exact(
                span,
                &format!("{}.{key}", sample.metadata_namespace),
                &expected,
            );
        }
        assert_eq!(span["traceId"].as_str().expect("trace id").len(), 32);
        assert_eq!(span["spanId"].as_str().expect("span id").len(), 16);
        assert!(!span["startTimeUnixNano"]
            .as_str()
            .expect("start")
            .is_empty());
        assert_eq!(
            measure_otlp(std::slice::from_ref(&event), usize::MAX).expect("measure"),
            body.len() as u64
        );
    }
    assert!(inline_samples >= 60 && spooled_samples >= 60);
}

#[test]
fn escaped_utf8_across_chunk_boundary() {
    let text = "前\"中😀\\\n\u{1}\u{1f}后";
    let bytes = text.as_bytes().to_vec();
    let expected = serde_json::to_vec(text).expect("serde escaping");
    for chunk in [1usize, 2, 3, 4, 5, 7, 17, 64, 64 * 1024] {
        let mut buffer = vec![0u8; chunk];
        let mut out = Vec::new();
        let mut reader: &[u8] = &bytes;
        super::json_string::write_json_string(&mut reader, &mut out, &mut buffer).expect("escape");
        assert_eq!(out, expected, "chunk={chunk} 必须与整块读入逐字节相同");
    }
    // 同一批样本的冻结字节在任意块大小下都必须与 serde 输出一致。
    for value in sample_values().iter().take(16) {
        let bytes = serde_json::to_vec(value).expect("frozen bytes");
        let expected =
            serde_json::to_vec(&String::from_utf8(bytes.clone()).expect("utf8")).expect("serde");
        for chunk in [1usize, 3, 64] {
            let mut buffer = vec![0u8; chunk];
            let mut out = Vec::new();
            let mut reader: &[u8] = &bytes;
            super::json_string::write_json_string(&mut reader, &mut out, &mut buffer)
                .expect("escape");
            assert_eq!(out, expected, "chunk={chunk} 保真失败：{value}");
        }
    }
}

#[test]
fn empty_chunk_buffer_is_a_config_error() {
    let mut out = Vec::new();
    let mut reader: &[u8] = b"{}";
    let error = super::json_string::write_json_string(&mut reader, &mut out, &mut []).unwrap_err();
    assert!(matches!(error, LangfuseError::Config(_)));
    assert!(out.is_empty(), "空缓冲不得先写出半个字面量");
}

#[test]
fn read_failure_is_classified_as_spool_error() {
    struct Failing;
    impl std::io::Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("snapshot reader failed"))
        }
    }
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 8];
    let error =
        super::json_string::write_json_string(&mut Failing, &mut out, &mut buffer).unwrap_err();
    // 端口错误按 `Spool` 分类上报；`source` 的诊断文本是端口契约的一部分（非正文）。
    match error {
        LangfuseError::Spool(SpoolError::Io { operation, .. }) => {
            assert_eq!(operation, "read frozen snapshot");
        }
        other => panic!("读失败必须分类为 Spool::Io，实际 {other}"),
    }
    assert!(!out.is_empty(), "前缀字节已交给下游 writer（正文未完成）");
}

#[test]
fn preparing_source_is_rejected_instead_of_encoding_empty_body() {
    let budget = budget(1024 * 1024);
    let cell = Arc::new(crate::snapshot::SnapshotCell {
        budget: Arc::clone(&budget),
        prepare_seq: 1,
        state: Mutex::new(crate::snapshot::SnapshotState::Preparing),
        cancelled: std::sync::atomic::AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
    });
    let event = event(json!({
        "type": "event-create", "id": "event", "timestamp": "1970-01-01T00:00:00Z",
        "body": {"id": "5555555555555555", "traceId": TRACE_ID},
    }));
    let event = TelemetryEvent::try_new(
        event,
        Some(InputSource::Preparing(crate::snapshot::SnapshotTicket {
            cell,
        })),
        None,
        &budget,
    )
    .expect("telemetry event");
    let mut body = Vec::new();
    let error = write_otlp(std::slice::from_ref(&event), &mut body).unwrap_err();
    assert!(matches!(error, LangfuseError::IngestionApi(_)));
    // 流式编码在遇到未就绪来源前已写出前缀，但不得写出空 stringValue，也不得收尾成完整正文。
    assert!(
        !String::from_utf8(body.clone())
            .expect("utf8")
            .contains(r#""stringValue":"""#),
        "未就绪来源不得编码成空 stringValue"
    );
    assert!(!body.ends_with(b"}]}]}"), "未就绪来源不得收尾成完整正文");
}

#[test]
fn measurement_and_writing_share_one_metering() {
    let budget = budget(1024 * 1024);
    let values = sample_values();
    let store = MemStore::new();
    let sample = sample(3, &values, &budget, &store, false);
    let event =
        TelemetryEvent::try_new(sample.event, sample.input, sample.output, &budget).expect("event");
    let body = write_one(&event);
    let written = body.len() as u64;
    for limit in [body.len(), body.len() + 1] {
        assert_eq!(
            measure_otlp(std::slice::from_ref(&event), limit).unwrap(),
            written
        );
    }
    let error = measure_otlp(std::slice::from_ref(&event), body.len() - 1).unwrap_err();
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == body.len() - 1
    ));
}

#[test]
fn spooled_and_inline_sources_share_one_escaping() {
    let budget = budget(1024 * 1024);
    let values = sample_values();
    let store = MemStore::new();
    let mut bodies = Vec::new();
    for spooled in [false, true] {
        let sample = sample(5, &values, &budget, &store, spooled);
        let event = TelemetryEvent::try_new(sample.event, sample.input, sample.output, &budget)
            .expect("event");
        bodies.push(write_one(&event));
    }
    assert_eq!(bodies[0], bodies[1], "inline 与 file 必须共用同一转义");
}

#[test]
fn claim_is_returned_and_object_stays_readable() {
    let budget = budget(1024 * 1024);
    let store = MemStore::new();
    let baseline = r#"{"大":"大"}"#.as_bytes();
    let object = publish(&store, &budget, baseline);
    let event = event(json!({
        "type": "trace-create", "id": "event", "timestamp": "1970-01-01T00:00:00Z",
        "body": {"id": TRACE_ID, "timestamp": "1970-01-01T00:00:00Z"},
    }));
    let event = TelemetryEvent::try_new(
        event,
        Some(InputSource::Ready(InputSnapshot::Spooled(object))),
        None,
        &budget,
    )
    .expect("event");
    let first = write_one(&event);
    // 二次读取需要再次取走交付句柄：成功即证明上一次已归还清理槽。
    let second = write_one(&event);
    assert_eq!(first, second, "同一对象必须可重复读取");
    assert_exact(&spans(&first)[0], "langfuse.trace.input", baseline);
}

#[test]
fn write_failure_never_exposes_the_body() {
    struct Rejecting;
    impl Write for Rejecting {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("下游失败"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let budget = budget(1024 * 1024);
    let event = event(json!({
        "type": "trace-create", "id": "event", "timestamp": "1970-01-01T00:00:00Z",
        "body": {"id": TRACE_ID, "input": "SECRET-MARKER",
            "timestamp": "1970-01-01T00:00:00Z"},
    }));
    let event = TelemetryEvent::try_new(event, None, None, &budget).expect("event");
    let error = write_otlp(std::slice::from_ref(&event), &mut Rejecting).unwrap_err();
    assert!(matches!(error, LangfuseError::IngestionApi(_)));
    assert!(!error.to_string().contains("SECRET-MARKER"));
}

// ─── 旧格式基线（5206 字节） ────────────────────────────────────────

/// 旧物化路径的固定基线：等价于对旧 `OtelTraceExportRequest` 的 serde 序列化。
///
/// 基线与旧 `client_export_test.rs` 用同一组事件采集（crate 0.2.0），是「none 逐字节不变」的判据。
const GOLDEN: &str = concat!(
    r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"peri-agent"}},{"key":"service.version","value":{"stringValue":"0.2.0"}}]},"scopeSpans":[{"scope":{"name":"lan"#,
    r#"gfuse-client","version":"0.2.0"},"spans":[{"traceId":"11111111111111111111111111111111","spanId":"8f6d1fa0aa6d1873","name":"根 trace \"引号\"","kind":1,"startTimeUnixNano":"1704067201000000000","endTimeU"#,
    r#"nixNano":"1704067201000000000","attributes":[{"key":"langfuse.session.id","value":{"stringValue":"sess-1"}},{"key":"langfuse.user.id","value":{"stringValue":"user-1"}},{"key":"langfuse.release","value"#,
    r#"":{"stringValue":"rel-1"}},{"key":"langfuse.version","value":{"stringValue":"v1"}},{"key":"langfuse.environment","value":{"stringValue":"prod"}},{"key":"langfuse.trace.tags","value":{"stringValue":"a,"#,
    r#"b,标签"}},{"key":"langfuse.trace.input","value":{"stringValue":"{\"arr\":[1,2.5,null,true],\"esc\":\"a\\\"b\\\\c\\n\\t\",\"nested\":{\"k\":\"v\"},\"文本\":\"中文😀\"}"}},{"key":"langfuse.trace.output","value"#,
    r#"":{"stringValue":"[\"out\",1]"}},{"key":"langfuse.trace.name","value":{"stringValue":"根 trace \"引号\""}},{"key":"langfuse.trace.metadata","value":{"stringValue":"{\"m_num\":7,\"m_obj\":{\"deep\":[1,2]}"#,
    r#",\"m_str\":\"值\"}"}},{"key":"langfuse.trace.metadata.m_num","value":{"stringValue":"7"}},{"key":"langfuse.trace.metadata.m_obj","value":{"stringValue":"{\"deep\":[1,2]}"}},{"key":"langfuse.trace.metad"#,
    r#"ata.m_str","value":{"stringValue":"值"}}],"status":{}},{"traceId":"11111111111111111111111111111111","spanId":"2222222222222222","parentSpanId":"3333333333333333","name":"span 名","kind":1,"startTimeUni"#,
    r#"xNano":"1704067202000000000","endTimeUnixNano":"1704067203250000000","attributes":[{"key":"langfuse.observation.type","value":{"stringValue":"span"}},{"key":"langfuse.observation.input","value":{"stri"#,
    r#"ngValue":"{\"in\":1}"}},{"key":"langfuse.observation.output","value":{"stringValue":"{\"out\":2}"}},{"key":"langfuse.observation.metadata","value":{"stringValue":"{\"源\":\"span\"}"}},{"key":"langfuse."#,
    r#"observation.metadata.源","value":{"stringValue":"span"}},{"key":"langfuse.version","value":{"stringValue":"v2"}},{"key":"langfuse.environment","value":{"stringValue":"dev"}},{"key":"langfuse.session.id"#,
    r#"","value":{"stringValue":"sess-2"}},{"key":"langfuse.observation.status_message","value":{"stringValue":"失败 \"原因\""}}],"status":{"code":2,"message":"失败 \"原因\""}},{"traceId":"11111111111111111111111111"#,
    r#"111111","spanId":"4444444444444444","name":"gen","kind":1,"startTimeUnixNano":"1704067204000000000","endTimeUnixNano":"1704067205000000000","attributes":[{"key":"langfuse.observation.type","value":{"s"#,
    r#"tringValue":"generation"}},{"key":"langfuse.observation.input","value":{"stringValue":"{\"messages\":[{\"content\":\"你好\",\"role\":\"user\"}]}"}},{"key":"langfuse.observation.output","value":{"stringV"#,
    r#"alue":"{\"content\":\"世界\"}"}},{"key":"langfuse.observation.metadata","value":{"stringValue":"{\"tokens\":10,\"标签\":\"好\"}"}},{"key":"langfuse.observation.metadata.tokens","value":{"stringValue":"10"}"#,
    r#"},{"key":"langfuse.observation.metadata.标签","value":{"stringValue":"好"}},{"key":"langfuse.environment","value":{"stringValue":"staging"}},{"key":"langfuse.observation.model.name","value":{"stringValue"#,
    r#"":"gpt-4o"}},{"key":"langfuse.observation.model.parameters","value":{"stringValue":"{\"temperature\":0.7}"}},{"key":"langfuse.observation.usage_details","value":{"stringValue":"{\"input\":10}"}},{"key"#,
    r#"":"gen_ai.usage.input","value":{"intValue":10}},{"key":"langfuse.observation.cost_details","value":{"stringValue":"{\"input_cost\":0.0001}"}},{"key":"langfuse.observation.prompt.name","value":{"string"#,
    r#"Value":"p1"}},{"key":"langfuse.observation.completion_start_time","value":{"stringValue":"2024-01-01T00:00:04.500Z"}},{"key":"langfuse.session.id","value":{"stringValue":"sess-3"}}],"status":{}},{"tra"#,
    r#"ceId":"11111111111111111111111111111111","spanId":"5555555555555555","name":"ev","kind":1,"startTimeUnixNano":"1704067206000000000","endTimeUnixNano":"1704067206000000000","attributes":[{"key":"langfu"#,
    r#"se.observation.type","value":{"stringValue":"event"}},{"key":"langfuse.observation.output","value":{"stringValue":"\"文本\""}},{"key":"langfuse.observation.metadata","value":{"stringValue":"{\"空\":\"\","#,
    r#"\"零\":0}"}},{"key":"langfuse.observation.metadata.空","value":{"stringValue":""}},{"key":"langfuse.observation.metadata.零","value":{"stringValue":"0"}},{"key":"langfuse.version","value":{"stringValue":"#,
    r#""v3"}}],"status":{}},{"traceId":"11111111111111111111111111111111","spanId":"6666666666666666","parentSpanId":"3333333333333333","name":"tool","kind":1,"startTimeUnixNano":"1704067207000000000","endTi"#,
    r#"meUnixNano":"1704067207000000000","attributes":[{"key":"langfuse.observation.type","value":{"stringValue":"tool"}},{"key":"langfuse.observation.input","value":{"stringValue":"[]"}},{"key":"langfuse.en"#,
    r#"vironment","value":{"stringValue":"prod"}},{"key":"langfuse.observation.model.name","value":{"stringValue":"gpt-4o-mini"}},{"key":"langfuse.observation.status_message","value":{"stringValue":"tool 失败""#,
    r#"}},{"key":"langfuse.session.id","value":{"stringValue":"sess-4"}}],"status":{"code":2,"message":"tool 失败"}}]}]}]}"#,
);

/// 六个事件覆盖全部 span 家族（sdk-log 被过滤），固定旧格式字节。
fn golden_events() -> Vec<IngestionEvent> {
    let mut params = HashMap::new();
    params.insert("temperature".to_string(), json!(0.7));
    let mut usage = HashMap::new();
    usage.insert("input".to_string(), json!(10));
    let mut usage_details = HashMap::new();
    usage_details.insert("input".to_string(), 10);
    let mut cost_details = HashMap::new();
    cost_details.insert("input_cost".to_string(), 0.0001f64);
    vec![
        event(json!({
            "type": "trace-create", "id": "trace-1", "timestamp": "2024-01-01T00:00:00Z",
            "body": {"id": TRACE_ID, "name": "根 trace \"引号\"", "userId": "user-1",
                "input": {"文本":"中文😀","esc":"a\"b\\c\n\t","arr":[1,2.5,null,true],
                    "nested":{"k":"v"}},
                "output": ["out",1], "sessionId": "sess-1", "release": "rel-1",
                "version": "v1", "metadata": {"m_str":"值","m_num":7,"m_obj":{"deep":[1,2]}},
                "tags": ["a,b","标签"], "environment": "prod",
                "timestamp": "2024-01-01T00:00:01Z"},
        })),
        event(json!({
            "type": "span-create", "id": "span-1", "timestamp": "2024-01-01T00:00:02Z",
            "body": {"id": "2222222222222222", "traceId": TRACE_ID, "name": "span 名",
                "startTime": "2024-01-01T00:00:02Z", "endTime": "2024-01-01T00:00:03.250Z",
                "input": {"in":1}, "output": {"out":2}, "metadata": {"源":"span"},
                "level": "ERROR", "statusMessage": "失败 \"原因\"",
                "parentObservationId": "3333333333333333", "version": "v2",
                "environment": "dev", "sessionId": "sess-2"},
        })),
        event(json!({
            "type": "generation-create", "id": "gen-1", "timestamp": "2024-01-01T00:00:04Z",
            "body": {"id": "4444444444444444", "traceId": TRACE_ID, "name": "gen",
                "startTime": "2024-01-01T00:00:04Z", "endTime": "2024-01-01T00:00:05Z",
                "input": {"messages":[{"role":"user","content":"你好"}]},
                "output": {"content":"世界"},
                "metadata": {"tokens":10,"标签":"好"}, "level": "DEBUG",
                "environment": "staging",
                "completionStartTime": "2024-01-01T00:00:04.500Z", "model": "gpt-4o",
                "modelParameters": params, "usage": usage, "usageDetails": usage_details,
                "costDetails": cost_details, "promptName": "p1", "sessionId": "sess-3"},
        })),
        event(json!({
            "type": "event-create", "id": "event-1", "timestamp": "2024-01-01T00:00:06Z",
            "body": {"id": "5555555555555555", "traceId": TRACE_ID, "name": "ev",
                "startTime": "2024-01-01T00:00:06Z", "output": "文本",
                "metadata": {"空":"","零":0}, "level": "WARNING", "statusMessage": "warn",
                "version": "v3"},
        })),
        event(json!({
            "type": "observation-create", "id": "obs-1", "timestamp": "2024-01-01T00:00:07Z",
            "body": {"id": "6666666666666666", "traceId": TRACE_ID, "type": "TOOL",
                "startTime": "2024-01-01T00:00:07Z",
                "parentObservationId": "3333333333333333", "input": [],
                "model": "gpt-4o-mini", "level": "ERROR", "statusMessage": "tool 失败",
                "environment": "prod", "sessionId": "sess-4"},
        })),
        event(json!({
            "type": "sdk-log", "id": "log-1", "timestamp": "2024-01-01T00:00:08Z",
            "body": {"log": {"m": "sdk"}},
        })),
    ]
}

/// none 编码路径与旧物化入口必须驱动同一 emitter，逐字节等同旧格式。
#[test]
fn frozen_baseline_bytes_are_stable() {
    let budget = budget(8 * 1024 * 1024);
    let events = golden_events();
    let telemetry: Vec<TelemetryEvent> = events
        .iter()
        .map(|event| {
            TelemetryEvent::try_new(event.clone(), None, None, &budget).expect("telemetry event")
        })
        .collect();
    let mut body = Vec::new();
    let written = write_otlp(&telemetry, &mut body).expect("write otlp");
    assert_eq!(written, body.len() as u64, "返回长度必须等于实际字节数");
    assert_eq!(
        String::from_utf8(body).expect("utf8"),
        GOLDEN,
        "none 编码必须逐字节等同旧格式"
    );
    let materialized = crate::types::ingestion_events_to_otel(&events).expect("materialized");
    assert_eq!(
        serde_json::to_vec(&materialized).expect("serde bytes"),
        GOLDEN.as_bytes(),
        "旧物化入口必须驱动同一 emitter"
    );
}
