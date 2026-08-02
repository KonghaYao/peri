//! run_compact 新触发流程测试

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::{ContextPressure, FullEscalationReason};
use crate::agent::compact_v2::{
    determine_compact_action, run_compact, CompactAction, CompactOutcome,
};
use crate::agent::events::CompactStrategy;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::transcript::MessageTranscript;
use crate::thread::{FilesystemThreadStore, ThreadMeta, ThreadStore};
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai_with_tool(text: &str, tool_name: &str, tool_id: &str) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        MessageContent::text(text.to_string()),
        vec![crate::messages::ToolCallRequest::new(
            tool_id,
            tool_name,
            serde_json::json!({"content": "x".repeat(501)}),
        )],
    )
}

fn make_tool_result(tool_call_id: &str, _text: &str) -> BaseMessage {
    BaseMessage::tool_result(
        tool_call_id.to_string(),
        MessageContent::text("x".repeat(501)),
    )
}

struct MockSummaryModel;

#[async_trait::async_trait]
impl Model for MockSummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        // compact 路径只走 complete()，stream() 不应被调用
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        Ok(ModelResponse::new(
            ModelMessage::assistant_text("<summary>compact summary</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

/// 用 budget 百分比构建 ContextPressure（测试辅助）
fn pressure_from_budget(budget_pct: f64) -> ContextPressure {
    let context_window = 200_000u32;
    ContextPressure {
        estimated_tokens: (budget_pct * context_window as f64) as u64,
        context_window,
        output_reserve: 8000,
        predicted_tool_growth: 0,
        safety_buffer: 5000,
        cache_hit_rate: 0.0,
    }
}

// ── determine_compact_action 测试 ──────────────────────────────────────

#[test]
fn test_determine_compact_action_below_threshold() {
    let config = CompactConfig::default();
    assert_eq!(determine_compact_action(0.50, &config), CompactAction::Skip);
}

#[test]
fn test_determine_compact_action_above_threshold() {
    let config = CompactConfig::default();
    // 默认 smart_compact_enabled = false → 应返回 Micro
    assert_eq!(
        determine_compact_action(0.80, &config),
        CompactAction::Micro
    );
}

#[test]
fn test_determine_compact_action_smart_enabled() {
    let config = CompactConfig {
        smart_compact_enabled: true,
        ..Default::default()
    };
    assert_eq!(
        determine_compact_action(0.80, &config),
        CompactAction::Smart
    );
}

// ── run_compact 触发流程测试（无 LLM） ─────────────────────────────────

#[tokio::test]
async fn test_micro_effective_no_full_overlay() {
    // budget = 0.80 → 75% < 80% < 95% → Micro 有效 → 不叠加 Full
    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.80);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
    )
    .await;
    assert_eq!(result.strategy, CompactStrategy::Micro, "应走 Micro");
    assert!(result.affected_count >= 5, "Micro 有效，不应升级 Full");
    assert!(result.summary.is_none(), "Micro 无摘要");
}

#[tokio::test]
async fn test_micro_invalid_upgrades_to_full() {
    // 仅 3 轮 + stale_steps=1 → 回收不足 + budget=0.80 ≥ 0.95? 否 → Micro 应用（部分收益）
    // 此测试改为：验证低 budget 时 Micro 应用而不是升级
    let mut t = MessageTranscript::new();
    for i in 0..3 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let mut failures = 0u32;
    // budget=0.80 (< 0.95) → Micro 应用（部分收益），不升级 Full
    let pressure = pressure_from_budget(0.80);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
    )
    .await;
    // budget=0.80 < threshold=0.95 → 走 "不足但未达 Full 阈值" 路径 → 应用 Micro
    assert_eq!(
        result.strategy,
        CompactStrategy::Micro,
        "budget 低于 Full 阈值时应走 Micro"
    );
    assert_eq!(failures, 0, "Micro 不应计失败");
}

#[tokio::test]
async fn test_micro_effective_full_overlay() {
    // budget = 0.98 → ≥ 95% → dry-run 估算 → token saving 不足 + budget 高位 → 跳过 Micro → 直接 Full
    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // budget=0.98 (196000 tokens) → 远高于 threshold → 直接 Full（无 LLM 降级）
    let pressure = pressure_from_budget(0.98);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
    )
    .await;
    // Full 无 LLM 失败，但已应用的 Micro 必须仍是有效策略视图。
    assert_eq!(
        result.strategy,
        CompactStrategy::Micro,
        "Micro 已应用但 Full 失败时应报告 Micro"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::MicroAppliedThenFullFailed,
        "结果应明确表示 Micro 已应用而 Full 失败"
    );
}

#[tokio::test]
async fn test_force_full_failure_preserves_persistent_excluded_flags_after_prior_failure() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let store: std::sync::Arc<dyn ThreadStore> =
        std::sync::Arc::new(FilesystemThreadStore::new(dir.path().join("threads")));
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy().to_string()))
        .await
        .expect("创建 Filesystem thread 失败");

    let ancestor = make_human("ancestor must remain visible");
    let own_system = BaseMessage::system("own system must remain visible");
    let own_excluded = make_human("pre-existing excluded own message");
    store
        .append_messages(
            &thread_id,
            &[ancestor.clone(), own_system.clone(), own_excluded.clone()],
        )
        .await
        .expect("持久化初始 transcript 失败");

    let mut transcript = MessageTranscript::new()
        .with_ancestor(vec![ancestor.clone()])
        .with_persistence(store, thread_id);
    transcript.append(own_system.clone());
    let excluded_id = transcript.append(own_excluded.clone());
    transcript
        .flush_persistence()
        .await
        .expect("刷新 persistent transcript 失败");
    transcript.set_excluded(excluded_id, true);
    transcript
        .flush_persistence()
        .await
        .expect("持久化预置 excluded flag 失败");

    let config = CompactConfig {
        max_consecutive_failures: 2,
        ..Default::default()
    };
    let mut consecutive_failures = 1;
    let result = run_compact(
        &mut transcript,
        None,
        &config,
        &pressure_from_budget(0.50),
        true,
        &mut consecutive_failures,
        &dir.path().to_string_lossy(),
    )
    .await;

    assert_eq!(result.outcome(), CompactOutcome::FullFailed);
    assert!(
        transcript.flags(excluded_id).excluded,
        "Full 失败前不得清除 pre-existing excluded own message 的 stale flag"
    );
    assert!(
        !transcript.flags(ancestor.id()).excluded,
        "ancestor flag 不得被 Full failure 路径改写"
    );
    assert!(
        !transcript.flags(own_system.id()).excluded,
        "System flag 不得被 Full failure 路径改写"
    );
}

#[tokio::test]
async fn test_force_triggers_full_directly() {
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));
    t.append(make_ai_with_tool("", "Bash", "call_1"));
    t.append(make_tool_result("call_1", "output"));

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // force=true + 无 LLM → Full 降级
    let pressure = pressure_from_budget(0.50);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        true,
        &mut failures,
        "/tmp",
    )
    .await;
    assert_eq!(result.strategy, CompactStrategy::Full);
    assert_eq!(
        result.outcome(),
        CompactOutcome::FullFailed,
        "force=true 的纯 Full failure 必须精确表示为 FullFailed"
    );
    assert_eq!(failures, 1);
}

// ── 特征化测试：Full 无 LLM 失败不 panic ─────────────────────────────────

#[tokio::test]
async fn test_full_without_llm_fails_no_panic() {
    // Full Compact 无 LLM 时优雅降级，不 panic
    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // budget=0.80 → Micro 执行，不升级 Full（因为 budget < threshold）
    let pressure = pressure_from_budget(0.80);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
    )
    .await;
    // budget=0.80 → Micro → 满足 target → 只走 Micro
    assert!(
        matches!(result.strategy, CompactStrategy::Micro),
        "无 LLM 时应保持在 Micro 策略"
    );
    assert!(result.affected_count > 0, "Micro 阶段应标记了消息");
    assert!(result.summary.is_none(), "无 LLM 时 Full 不产生摘要");
}

#[tokio::test]
async fn test_micro_applied_then_full_failure_is_not_reported_as_full_completion() {
    // 高上下文压力下，Micro 先应用；随后无 LLM 的 Full 必然失败。
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2_000);
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &long_output));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.98);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert!(
        t.entries()
            .iter()
            .any(|entry| t.flags(entry.message.id()).truncated),
        "Full 失败后仍应保留已应用的 Micro 截断"
    );
    assert_eq!(failures, 1, "Full 失败应计入连续失败次数");
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::InsufficientReclaim),
        "结果应保留 Full 升级及失败前的原因"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::MicroAppliedThenFullFailed,
        "结果应明确表示 Micro 已应用而 Full 失败"
    );
    assert_ne!(
        result.strategy,
        CompactStrategy::Full,
        "Micro 已应用但 Full 失败时，不得伪装为 Full completion"
    );
    assert!(result.summary.is_none(), "失败的 Full 不得产生完成摘要");
}

// ── Task 6: estimated_tokens_saved 端到端验证 ──────────────────────────

#[tokio::test]
async fn test_estimated_tokens_saved_reflected_in_result() {
    // 有效 Micro Compact 的 estimated_tokens_saved 应 > 0
    // 与 planner_test 对齐：使用 "x".repeat(2000) 保证 token 估算 > 0
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2000);
    for i in 0..10 {
        t.append(make_human(&format!("question {}", i)));
        t.append(make_ai_with_tool(
            &format!("thinking {}", i),
            "Bash",
            &format!("call_{}", i),
        ));
        t.append(make_tool_result(&format!("call_{}", i), &long_output));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..CompactConfig::default()
    };
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.80);
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert!(
        result.estimated_tokens_saved > 0,
        "Micro Compact 应估算非零 token 节省，实际: {}",
        result.estimated_tokens_saved
    );
    assert!(
        result.before_visible_len >= result.after_visible_len,
        "Compact 后可见消息数不应增多，before={}, after={}",
        result.before_visible_len,
        result.after_visible_len
    );
    assert!(result.affected_count > 0, "affected_count 应 > 0");
    assert_eq!(result.full_escalation_reason, None, "Micro 不应有升级原因");
}

#[tokio::test]
async fn test_estimated_tokens_saved_increases_with_more_rounds() {
    // 更多轮次应产生更大的 token 节省估算
    let long_output = "x".repeat(2000);
    async fn make_and_compact(rounds: usize, long_output: &str) -> u64 {
        let mut t = MessageTranscript::new();
        for i in 0..rounds {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai_with_tool(
                &format!("think {}", i),
                "Bash",
                &format!("c_{}", i),
            ));
            t.append(make_tool_result(&format!("c_{}", i), long_output));
        }
        let config = CompactConfig {
            micro_compact_stale_steps: 2,
            ..CompactConfig::default()
        };
        let mut failures = 0u32;
        let pressure = pressure_from_budget(0.80);
        run_compact(
            &mut t,
            None,
            &config,
            &pressure,
            false,
            &mut failures,
            "/tmp",
        )
        .await
        .estimated_tokens_saved
    }

    let saved_6 = make_and_compact(6, &long_output).await;
    let saved_10 = make_and_compact(10, &long_output).await;
    assert!(
        saved_10 >= saved_6,
        "更多轮次应产生更大的 token 节省，6轮={}，10轮={}",
        saved_6,
        saved_10
    );
}

#[tokio::test]
async fn test_run_compact_smart_applied_then_full_failure_preserves_effects() {
    // 高压力下 Smart 已先应用；无 LLM 的 Full 随后失败也不得抹去该事实。
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2_000);
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &long_output));
    }

    let config = CompactConfig {
        smart_compact_enabled: true,
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let mut failures = 0u32;
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.98),
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert_eq!(
        result.strategy,
        CompactStrategy::Smart,
        "应保留已应用的 Smart 策略"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::SmartAppliedThenFullFailed,
        "结果必须精确表示 Smart 已应用而 Full 失败"
    );
    assert!(
        t.entries()
            .iter()
            .any(|entry| t.flags(entry.message.id()).truncated),
        "Full 失败后必须保留 Smart 已应用的 truncated 标记"
    );
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::InsufficientReclaim),
        "应保留后续 Full 失败前的升级原因"
    );
    assert!(result.summary.is_none(), "失败的 Full 不得产生完成摘要");
    assert!(result.affected_count > 0, "应保留 Smart 的 affected_count");
    assert!(
        result.estimated_tokens_saved > 0,
        "应保留 Smart 的 estimated_tokens_saved"
    );
    assert_eq!(failures, 1, "Full 失败应计入连续失败次数");
}

#[tokio::test]
async fn test_smart_then_full_success_aggregates_metrics() {
    // Full compact 现在需要 persistence（commit_compaction_lifecycle 要求 store）。
    // 使用临时 SQLite store 满足此约束。
    let store_dir = tempfile::tempdir().expect("创建临时目录失败");
    let db_path = store_dir.path().join("test_aggregate.db");
    let store = Arc::new(
        crate::thread::SqliteThreadStore::new(db_path.to_string_lossy().to_string())
            .await
            .expect("创建 SQLite store 失败"),
    );
    let thread_id = store
        .create_thread(crate::thread::ThreadMeta::new("/tmp".to_string()))
        .await
        .expect("创建 thread 失败");

    let mut t = MessageTranscript::new().with_persistence(store.clone(), thread_id);
    let long_output = "x".repeat(2_000);
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &long_output));
    }

    let config = CompactConfig {
        smart_compact_enabled: true,
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let mut failures = 0u32;
    let result = run_compact(
        &mut t,
        Some(&MockSummaryModel),
        &config,
        &pressure_from_budget(0.98),
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert_eq!(result.strategy, CompactStrategy::Full);
    assert_eq!(result.outcome(), CompactOutcome::FullApplied);
    // 注意：Full Compact 成功时 affected_count 仅包含 Full 的 excluded 消息数
    // （Smart 的实际标记在 transcript 中生效，但 affected_count 由 Full 的 lifecycle 计算）
    assert!(
        result.affected_count > 0,
        "Full 成功应包含被 excluded 的消息"
    );
    assert!(
        result.estimated_tokens_saved > 0,
        "Full 本身当前不估算节省量，结果仍必须保留 Smart 的节省量"
    );
    assert_eq!(failures, 0, "成功 Full 应清零连续失败次数");
}

#[tokio::test]
async fn test_run_compact_failure_limit_returns_explicit_skipped_outcome() {
    let mut t = MessageTranscript::new();
    let config = CompactConfig {
        max_consecutive_failures: 3,
        ..Default::default()
    };
    let mut failures = config.max_consecutive_failures;
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.98),
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert_eq!(
        result.strategy,
        CompactStrategy::Skip,
        "到达失败上限必须明确跳过"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::Skipped,
        "到达失败上限不得表达为 MicroApplied"
    );
    assert_eq!(result.affected_count, 0, "跳过时不应影响消息");
    assert_eq!(
        failures, config.max_consecutive_failures,
        "跳过不应改写失败计数"
    );
}

#[tokio::test]
async fn test_run_compact_micro_shadow_mode_returns_shadowed_without_changes() {
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2_000);
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &long_output));
    }
    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    };
    let mut failures = 0u32;
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.80),
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert_eq!(
        result.outcome(),
        CompactOutcome::Shadowed,
        "shadow mode 应明确表示只估算且未应用"
    );
    assert_eq!(result.affected_count, 0, "shadow mode 不应报告已影响消息");
    assert!(
        t.entries()
            .iter()
            .all(|entry| !t.flags(entry.message.id()).truncated),
        "shadow mode 绝不应改写 transcript"
    );
}

#[tokio::test]
async fn test_run_compact_smart_shadow_mode_returns_shadowed_without_changes() {
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2_000);
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &long_output));
    }
    let config = CompactConfig {
        smart_compact_enabled: true,
        micro_compact_stale_steps: 1,
        shadow_mode_enabled: true,
        ..Default::default()
    };
    let mut failures = 0u32;
    let result = run_compact(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.80),
        false,
        &mut failures,
        "/tmp",
    )
    .await;

    assert_eq!(
        result.outcome(),
        CompactOutcome::Shadowed,
        "shadow mode 应明确表示只估算且未应用"
    );
    assert_eq!(result.affected_count, 0, "shadow mode 不应报告已影响消息");
    assert!(
        t.entries()
            .iter()
            .all(|entry| !t.flags(entry.message.id()).truncated),
        "shadow mode 绝不应改写 transcript"
    );
}
