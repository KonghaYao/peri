//! LLM Generation 追踪器单元测试。
//!
//! 追踪器只搬运不透明的已冻结来源：不序列化正文、不接触文件路径。因此顺序/重试/键隔离用例用
//! [`GenerationInput::Rejected`]（冻结被拒时控制器持有的变体，无需部署基础设施即可构造）作为
//! 载荷；输入正文与冻结语义由 `input_snapshot_test.rs` 的真实 Batcher → OTLP 逐字节核对覆盖。

use std::sync::Arc;
use std::time::Duration;

use langfuse_client::client::ExportConfig;
use langfuse_client::config::{BatcherConfig, Compression, TelemetryConfig};
use langfuse_client::snapshot::snapshot_value;
use langfuse_client::{Batcher, LangfuseClient, LangfuseError};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

use super::super::input_snapshot::{llm_input_source, GenerationInput};
use super::*;

/// 与本用例无关的载荷占位；追踪器不解释来源内容，只负责移动与释放。
fn opaque_input() -> GenerationInput {
    GenerationInput::Rejected
}

#[test]
fn test_on_llm_start_sets_active_step() {
    let mut t = GenerationTracker::new();
    let start = t.on_llm_start("main", 0, opaque_input());
    assert!(start.gen_id.starts_with("gen_"));
}

#[test]
fn test_on_llm_end_returns_generation_end_and_clears_state() {
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, opaque_input());
    let end = t.on_llm_end("main", 0).expect("should return Some");
    assert!(end.gen_id.starts_with("gen_"));
    // 再次 on_llm_end 应返回 None
    assert!(t.on_llm_end("main", 0).is_none());
}

#[test]
fn test_on_llm_retrying_accumulates_attempts() {
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, opaque_input());
    t.on_llm_retrying("main", 0, 1, 3, 1000, "timeout");
    t.on_llm_retrying("main", 0, 2, 3, 2000, "timeout");
    let end = t.on_llm_end("main", 0).expect("should return Some");
    assert!(end.retry_metadata.is_some());
    let meta = end.retry_metadata.unwrap();
    assert_eq!(meta["retry_count"], 2);
    assert_eq!(meta["retries"][0]["delay_ms"], 1000);
    assert!(meta.get("error").is_none());
}

#[test]
fn test_on_llm_start_clears_previous_retry_attempts() {
    // 第二次 on_llm_start 应清空 retry_attempts（按 generation key 隔离）
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, opaque_input());
    t.on_llm_retrying("main", 0, 1, 3, 1000, "err");
    t.on_llm_start("main", 1, opaque_input()); // 新 step
    let end = t.on_llm_end("main", 1).expect("should return Some");
    assert!(end.retry_metadata.is_none(), "新 step 不应携带旧 retry");
}

#[test]
fn test_interleaved_agents_keep_their_own_retries() {
    // 并行 agent 交错：A start → B start → A retry → A end → B retry → B end
    // 每个 end 只能消费自己 generation 的 retry 历史
    let mut t = GenerationTracker::new();
    t.on_llm_start("agent_a", 5, opaque_input());
    t.on_llm_start("agent_b", 1, opaque_input());
    t.on_llm_retrying("agent_a", 5, 1, 3, 500, "a-timeout");
    t.on_llm_retrying("agent_a", 5, 2, 3, 1000, "a-timeout");
    // A 先 end：只应携带 A 自己的 retry，不能消费到 B 的
    let end_a = t
        .on_llm_end("agent_a", 5)
        .expect("A end should return Some");
    let meta_a = end_a.retry_metadata.expect("A should carry its retries");
    assert_eq!(meta_a["retry_count"], 2);
    assert_eq!(meta_a["retries"][0]["delay_ms"], 500);
    assert!(meta_a.get("error").is_none());
    // B 后 end：只应携带 B 自己的 retry
    t.on_llm_retrying("agent_b", 1, 1, 2, 800, "b-timeout");
    let end_b = t
        .on_llm_end("agent_b", 1)
        .expect("B end should return Some");
    let meta_b = end_b.retry_metadata.expect("B should carry its retries");
    assert_eq!(meta_b["retry_count"], 1);
    assert_eq!(meta_b["retries"][0]["delay_ms"], 800);
    assert!(meta_b.get("error").is_none());
}

#[test]
fn test_on_llm_end_unknown_step_returns_none() {
    let mut t = GenerationTracker::new();
    assert!(t.on_llm_end("main", 99).is_none());
}

// ── 冻结来源的所有权（真实 Batcher 冻结，正文核对见 input_snapshot_test.rs） ──────

/// 真实冻结 fixture：来源与 permit 不可伪造，因此冻结只能由真实 `Batcher` 产出。
struct Frozen {
    input: GenerationInput,
    batcher: Arc<Batcher>,
}

/// 冻结借用适配器产出的来源。`#[tokio::test]` 的单线程 runtime 保证未 await 前 worker 不推进，
/// 冻结后到 `flush` 前的借用计数因此可确定核对。
fn freeze(messages: &Arc<Vec<BaseMessage>>, tools: &Arc<Vec<ToolDefinition>>) -> Frozen {
    let client = LangfuseClient::new("pk-generation", "sk-generation", "http://127.0.0.1:9", 1)
        .with_export_config(ExportConfig {
            compression: Compression::None,
            ..Default::default()
        })
        .expect("export config");
    let config = BatcherConfig {
        telemetry: TelemetryConfig {
            compression: Compression::None,
            ..Default::default()
        },
        flush_interval: Duration::from_secs(3600),
        ..Default::default()
    };
    let batcher = Arc::new(Batcher::try_new(client, config, None).expect("batcher"));
    Frozen {
        input: GenerationInput::Frozen(
            batcher
                .try_freeze(llm_input_source(messages, tools))
                .expect("freeze"),
        ),
        batcher,
    }
}

fn snapshot() -> (Arc<Vec<BaseMessage>>, Arc<Vec<ToolDefinition>>) {
    let messages = Arc::new(vec![BaseMessage::human("完整上下文🙂".repeat(4096))]);
    let tools = Arc::new(vec![ToolDefinition {
        name: "lookup".to_string(),
        description: "lookup description".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    }]);
    (messages, tools)
}

/// 排空直接驱动 tracker 时产生的冻结来源，并核对准备账本。
///
/// 本文件不经 `TurnTracer` 事件层，冻结出来的来源不会被任何事件引用（`try_add_prepared` 才登记
/// 依赖）；worker 排空这类语义上「无事件」的准备时，按 client 契约记入独立准备失败账本，flush 返回
/// `SnapshotPreparationFailed`，而不是静默当作成功。这里据此断言两种终态：要么全部准备成功，
/// 要么恰好是这 `unattached` 条无事件准备失败——两者都要求 Preparation 已 join、借用已归还。
async fn flush_unattached(batcher: &Batcher, unattached: u64) {
    match batcher.flush().await {
        Ok(_) => {}
        Err(LangfuseError::SnapshotPreparationFailed {
            failed_snapshots, ..
        }) => assert_eq!(
            failed_snapshots, unattached,
            "只允许无事件准备失败，且条数等于未被事件引用的冻结数"
        ),
        Err(error) => panic!("flush 不应出现其他失败: {error:?}"),
    }
}

#[tokio::test]
async fn fallback_source_is_borrowed_once_and_handed_out_at_end() {
    let mut tracker = GenerationTracker::new();
    let (messages, tools) = snapshot();
    let frozen = freeze(&messages, &tools);
    assert_eq!(
        Arc::strong_count(&messages),
        2,
        "冻结只借用原 Arc，不复制正文"
    );
    tracker.on_llm_start("main", 0, frozen.input);
    let end = tracker.on_llm_end("main", 0).expect("should return Some");
    assert!(
        matches!(end.input, GenerationInput::Frozen(_)),
        "结束事件必须拿到已冻结来源（正文逐字节核对见 input_snapshot_test.rs）"
    );
    drop(end);
    flush_unattached(&frozen.batcher, 1).await;
    assert_eq!(
        Arc::strong_count(&messages),
        1,
        "交付后不得残留借用（worker 排空被取消的准备请求）"
    );
}

#[tokio::test]
async fn request_payload_replaces_the_frozen_fallback() {
    let mut tracker = GenerationTracker::new();
    let (messages, tools) = snapshot();
    let frozen = freeze(&messages, &tools);
    tracker.on_llm_start("main", 0, frozen.input);
    // 替换来源即使冻结被拒也必须生效：旧 fallback 不得留在缓存里冒充实际请求
    tracker.on_llm_request_payload("main", 0, GenerationInput::Rejected);
    let end = tracker.on_llm_end("main", 0).expect("should return Some");
    assert!(
        end.input.is_rejected(),
        "请求体到达后必须替换掉 fallback 来源"
    );
    drop(end);
    flush_unattached(&frozen.batcher, 1).await;
    assert_eq!(
        Arc::strong_count(&messages),
        1,
        "被替换的 fallback 必须释放"
    );
}

#[tokio::test]
async fn raw_payload_source_is_held_by_reference_and_releases_the_fallback() {
    let mut tracker = GenerationTracker::new();
    let (messages, tools) = snapshot();
    let frozen = freeze(&messages, &tools);
    tracker.on_llm_start("main", 0, frozen.input);
    let raw = Arc::new(serde_json::json!({
        "model": "provider-model",
        "messages": [{"role": "user", "content": "raw🙂".repeat(4096)}],
        "tools": [{"name": "lookup", "parameters": {"type": "object"}}],
        "provider_extra": {"enabled": true},
    }));
    let payload = GenerationInput::Frozen(
        frozen
            .batcher
            .try_freeze(snapshot_value(&raw))
            .expect("freeze payload"),
    );
    assert_eq!(
        Arc::strong_count(&raw),
        2,
        "请求体按 Arc 借用冻结，不复制也不序列化正文"
    );
    tracker.on_llm_request_payload("main", 0, payload);
    let end = tracker.on_llm_end("main", 0).expect("should return Some");
    assert!(
        matches!(end.input, GenerationInput::Frozen(_)),
        "结束事件交付请求体来源（字节核对见 input_snapshot_test.rs）"
    );
    drop(end);
    flush_unattached(&frozen.batcher, 2).await;
    assert_eq!(Arc::strong_count(&raw), 1, "交付后释放请求体借用");
    assert_eq!(
        Arc::strong_count(&messages),
        1,
        "被替换的 fallback 不得残留"
    );
}

#[tokio::test]
async fn abandoned_generation_hands_out_source_and_retry_metadata_once() {
    let mut tracker = GenerationTracker::new();
    let (messages, tools) = snapshot();
    let frozen = freeze(&messages, &tools);
    tracker.on_llm_start("child", 3, frozen.input);
    tracker.on_llm_retrying("child", 3, 1, 2, 20, "private provider body");
    let abandoned = tracker.take_all_active();
    assert_eq!(abandoned.len(), 1);
    assert!(
        matches!(&abandoned[0].input, GenerationInput::Frozen(_)),
        "放弃的 generation 必须交出已冻结来源"
    );
    assert_eq!(
        abandoned[0].retry_metadata.as_ref().unwrap()["retry_count"],
        1
    );
    assert!(tracker.take_all_active().is_empty());
}
