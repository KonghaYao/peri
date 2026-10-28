//! run_compact 新触发流程测试

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::{
    plan_micro_call_count, reset_plan_micro_calls, ArchivePlanningScope, ContextPressure,
    FullEscalationReason, MicroPlanningFacts,
};
use crate::agent::compact_v2::{
    determine_compact_action, run_compact, run_compact_with_facts, CompactAction, CompactOutcome,
};
use crate::agent::events::CompactStrategy;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::test_resources::TestSession;
use crate::session::transcript::MessageTranscript;
use crate::thread::ThreadMeta;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
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

fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
    // Even the short fixture outputs carry enough body to pay the real archive receipt cost.
    // 8 轮左右的输出必须显著超过 200k 窗口的 20k 净收益门槛，否则统一准入会延后。
    let body = format!(
        "{text}{}",
        "x".repeat(24_000usize.saturating_sub(text.len()))
    );
    BaseMessage::tool_result(tool_call_id.to_string(), MessageContent::text(body))
}

fn exposed_facts(transcript: &MessageTranscript, session_id: &str) -> MicroPlanningFacts {
    let mut facts = MicroPlanningFacts {
        archive_scope: transcript
            .entries()
            .last()
            .map(|entry| ArchivePlanningScope {
                thread_id: session_id.into(),
                committed_tail: entry.id(),
            }),
        ..Default::default()
    };
    for entry in transcript.entries().iter().skip(transcript.ancestor_len()) {
        facts.exposed_message_ids.insert(entry.id());
        if let Some(message) = entry.as_message() {
            for call in message.tool_calls() {
                facts.closed_call_ids.insert(call.id.clone());
                facts.tool_retention.insert(
                    call.name.to_lowercase(),
                    crate::tools::ContextRetention::Recomputable,
                );
            }
        }
    }
    facts
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
        // High-pressure fixtures reserve a large next response, making Micro
        // savings insufficient for the requested Full-escalation branch.
        output_reserve: if budget_pct >= 0.95 { 40_000 } else { 8_000 },
        predicted_tool_growth: 0,
        safety_buffer: 5000,
        cache_hit_rate: 0.0,
    }
}

/// 断言本轮 run_compact 只规划了一次 Micro 计划（dry-run 计划被 apply 复用）。
fn assert_planned_once() {
    assert_eq!(
        plan_micro_call_count(),
        1,
        "Micro/Smart 计划每轮只应规划一次，应用必须复用 dry-run 计划"
    );
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
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.80);
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;
    assert_eq!(result.strategy, CompactStrategy::Micro, "应走 Micro");
    assert!(result.affected_count >= 5, "Micro 有效，不应升级 Full");
    assert!(result.summary.is_none(), "Micro 无摘要");
    assert_planned_once();
}

#[tokio::test]
async fn test_micro_below_gain_gate_defers_without_writes() {
    // 新契约：进入 Micro 区间但净收益低于完整窗口 10% 时主动延后，
    // 不再“部分收益也好”地提交低收益 Micro。
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    let pressure = pressure_from_budget(0.80);
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;
    assert_eq!(
        result.strategy,
        CompactStrategy::Micro,
        "区间策略标签仍为 Micro"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::Deferred,
        "收益不足应延后而不是提交"
    );
    assert_eq!(failures, 0, "延后不增加失败计数");
    assert_eq!(result.affected_count, 0, "延后不得写入");
    assert_eq!(result.changed_messages, 0, "延后不得写入");
    assert!(
        !t.entries()
            .iter()
            .any(|entry| t.flags(entry.id()).truncated || t.flags(entry.id()).projection.is_some()),
        "延后不得持久化任何投影标记"
    );
    let deferral = result.deferral.expect("延后必须携带结构化原因");
    assert_eq!(
        deferral.min_gain_tokens,
        crate::agent::compact_v2::min_micro_gain_tokens(200_000, &config)
    );
    assert!(deferral.estimated_gain_tokens < deferral.min_gain_tokens);
    assert_planned_once();
}

#[tokio::test]
async fn test_full_condition_does_not_commit_micro_before_full() {
    // 新契约：达到 Full 条件且 G < max(M, R) 时直接 Full，本轮不先提交 Micro。
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // budget=0.98 (196000 tokens) 且 R 很大 → G < max(M, R) → 直接 Full（无 LLM 失败）
    let pressure = pressure_from_budget(0.98);
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;
    assert_eq!(
        result.strategy,
        CompactStrategy::Full,
        "达到 Full 条件时不得把结果伪装成 Micro"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::FullFailed,
        "Full 失败必须显式报告，且本轮 Micro 提交为 0"
    );
    assert_eq!(result.affected_count, 0, "本轮 Micro 不得提交");
    assert_eq!(result.changed_messages, 0, "本轮 Micro 不得提交");
    assert!(
        !t.entries()
            .iter()
            .any(|entry| t.flags(entry.id()).truncated || t.flags(entry.id()).projection.is_some()),
        "Full 失败前不得先写 Micro 投影"
    );
    assert_planned_once();
}

#[tokio::test]
async fn test_micro_then_full_success_does_not_double_count_affected_messages() {
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }
    let expected_full_transitions = t.visible_messages().len();
    let mut failures = 0u32;

    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        Some(&MockSummaryModel),
        &CompactConfig::default(),
        &pressure_from_budget(0.98),
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;

    assert_eq!(result.outcome(), CompactOutcome::FullApplied);
    assert_eq!(
        result.affected_count, expected_full_transitions,
        "Full 成功时不得再次累加已由 excluded transition 覆盖的 Micro affected"
    );
}

#[tokio::test]
async fn test_force_full_failure_preserves_persistent_excluded_flags_after_prior_failure() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at(
            dir.path().to_string_lossy().to_string(),
            peri_time::now_wall(),
        ))
        .await
        .expect("创建 thread 失败");

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
        None,
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
        None,
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
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // budget=0.80 → Micro 执行，不升级 Full（因为 budget < threshold）
    let pressure = pressure_from_budget(0.80);
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
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
async fn test_full_failure_before_commit_leaves_no_hidden_micro_commit() {
    // 新契约：高压 Full 失败或提交前取消时，不得补交本轮被拒绝的 Micro。
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;

    assert!(
        !t.entries()
            .iter()
            .any(|entry| t.flags(entry.id()).truncated || t.flags(entry.id()).projection.is_some()),
        "Full 失败前不得留下本轮 Micro 的隐藏提交"
    );
    assert_eq!(failures, 1, "Full 失败应计入连续失败次数");
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::InsufficientReclaim),
        "结果应保留 Full 升级及失败前的原因"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::FullFailed,
        "Full 失败必须显式报告"
    );
    assert_eq!(result.strategy, CompactStrategy::Full);
    assert!(result.summary.is_none(), "失败的 Full 不得产生完成摘要");
}

// ── Task 6: estimated_tokens_saved 端到端验证 ──────────────────────────

#[tokio::test]
async fn test_estimated_tokens_saved_reflected_in_result() {
    // 有效 Micro Compact 的 estimated_tokens_saved 应 > 0
    // 与 planner_test 对齐：使用 "x".repeat(2000) 保证 token 估算 > 0
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
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
        let bound = TestSession::open().await;
        let mut t =
            MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
        let facts = exposed_facts(&t, &bound.thread_id);
        run_compact_with_facts(
            &mut t,
            None,
            &config,
            &pressure,
            false,
            &mut failures,
            "/tmp",
            None,
            &crate::session::async_result_mailbox::ProtectedResults::default(),
            &facts,
            &crate::agent::compact_v2::ProviderCapabilities::openai(),
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
async fn test_run_compact_smart_uses_same_admission_and_never_precommits() {
    // 新契约：Smart 打开时与 Micro 使用同一收益准入；高压 Full 失败前不得先写 Smart。
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.98),
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;

    assert_eq!(
        result.strategy,
        CompactStrategy::Full,
        "Smart 打开时也必须直接进入 Full，不得先写后判"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::FullFailed,
        "结果必须精确表示 Full 失败"
    );
    assert_planned_once();
    assert!(
        !t.entries()
            .iter()
            .any(|entry| t.flags(entry.id()).truncated || t.flags(entry.id()).projection.is_some()),
        "Full 失败前不得留下 Smart 的投影写入"
    );
    assert_eq!(
        result.full_escalation_reason,
        Some(FullEscalationReason::InsufficientReclaim),
        "应保留后续 Full 失败前的升级原因"
    );
    assert!(result.summary.is_none(), "失败的 Full 不得产生完成摘要");
    assert_eq!(result.affected_count, 0, "不得保留先写的 Smart 变更");
    assert_eq!(failures, 1, "Full 失败应计入连续失败次数");
}

#[tokio::test]
async fn test_smart_full_success_has_no_precommitted_micro_metrics() {
    // Full compact 现在需要 persistence（commit_compaction_lifecycle 要求 store）。
    // 使用临时 SQLite store 满足此约束。
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        Some(&MockSummaryModel),
        &config,
        &pressure_from_budget(0.98),
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;

    assert_eq!(result.strategy, CompactStrategy::Full);
    assert_eq!(result.outcome(), CompactOutcome::FullApplied);
    assert!(
        result.affected_count > 0,
        "Full 成功应包含被 excluded 的消息"
    );
    assert_eq!(
        result.estimated_tokens_saved, 0,
        "Full 本身不估算节省量；本轮没有先提交的 Smart 收益"
    );
    assert_eq!(failures, 0, "成功 Full 应清零连续失败次数");
}

#[tokio::test]
async fn test_run_compact_failure_limit_returns_explicit_full_failure() {
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
        None,
    )
    .await;

    assert_eq!(
        result.strategy,
        CompactStrategy::Full,
        "需要 Full 且已达失败上限必须明确失败"
    );
    assert_eq!(
        result.outcome(),
        CompactOutcome::FullFailed,
        "到达失败上限不得表达为 MicroApplied"
    );
    assert_eq!(result.affected_count, 0, "跳过时不应影响消息");
    assert!(matches!(
        result.failure,
        Some(crate::error::AgentError::CompactRetriesExhausted {
            attempts: 3,
            context_tokens: 196_000,
            context_window: 200_000
        })
    ));
    assert_eq!(
        failures, config.max_consecutive_failures,
        "跳过不应改写失败计数"
    );
}

#[tokio::test]
async fn test_run_compact_micro_shadow_mode_returns_shadowed_without_changes() {
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.80),
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
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
            .all(|entry| !t.flags(entry.id()).truncated),
        "shadow mode 绝不应改写 transcript"
    );
    assert_planned_once();
}

#[tokio::test]
async fn test_run_compact_smart_shadow_mode_returns_shadowed_without_changes() {
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
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
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure_from_budget(0.80),
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
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
            .all(|entry| !t.flags(entry.id()).truncated),
        "shadow mode 绝不应改写 transcript"
    );
    assert_planned_once();
}

/// Micro 计划达标（estimated_tokens_saved ≥ reclaim_target）时：
/// 只走 Micro、不叠加 Full，且计划只规划一次。
#[tokio::test]
async fn test_micro_satisfied_reclaim_applies_once_from_dry_run_plan() {
    let bound = TestSession::open().await;
    let mut t =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    // 本用例的 save 量必须真正超过 reclaim_target，因此显式提供更大的输出。
    let long_output = "x".repeat(20_000);
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(BaseMessage::tool_result(
            format!("c_{}", i),
            MessageContent::text(long_output.clone()),
        ));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.80);
    reset_plan_micro_calls();
    let facts = exposed_facts(&t, &bound.thread_id);
    let result = run_compact_with_facts(
        &mut t,
        None,
        &config,
        &pressure,
        false,
        &mut failures,
        "/tmp",
        None,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        &facts,
        &crate::agent::compact_v2::ProviderCapabilities::openai(),
    )
    .await;

    assert!(
        result.estimated_tokens_saved >= pressure.target_reclaim_tokens(),
        "本场景应满足回收目标，实际 saved={} target={}",
        result.estimated_tokens_saved,
        pressure.target_reclaim_tokens()
    );
    assert_eq!(result.outcome(), CompactOutcome::MicroApplied);
    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert_eq!(failures, 0, "达标的 Micro 不应计失败");
    assert!(
        result.affected_count > 0,
        "达标路径必须真实应用计划（affected_count > 0）"
    );
    assert_planned_once();
}
