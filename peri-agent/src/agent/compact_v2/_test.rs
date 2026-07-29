//! run_compact 集成测试
//!
//! 测试顶层入口 run_compact 在不同条件下的行为。

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::ContextPressure;
use crate::agent::compact_v2::run_compact;
use crate::agent::events::CompactStrategy;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::transcript::MessageTranscript;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
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

// ── run_compact 集成测试（无 LLM） ────────────────────────────────────────

#[tokio::test]
async fn test_run_compact_low_budget_skips() {
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));
    t.append(make_ai("answer"));

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.5);
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
    assert_eq!(result.affected_count, 0, "低预算应跳过");
}

#[tokio::test]
async fn test_run_compact_micro_threshold() {
    let mut t = MessageTranscript::new();
    // 构造足够多的轮次以触发 micro
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // budget = 0.80 → ≥ 0.75 → Micro
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
    assert!(result.affected_count >= 5, "Micro 有效，不应升级 Full");
}

#[tokio::test]
async fn test_run_compact_full_no_llm_fails_gracefully() {
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));
    t.append(make_ai("answer"));

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // force=true 但无 LLM → 失败降级
    let pressure = pressure_from_budget(0.5);
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
    assert_eq!(result.affected_count, 0, "失败时应无变更");
    assert_eq!(failures, 1, "失败计数应递增");
}

#[tokio::test]
async fn test_run_compact_consecutive_failure_degradation() {
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));

    let config = CompactConfig::default();
    let mut failures = config.max_consecutive_failures; // 已达上限
    let pressure = pressure_from_budget(0.95);
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
    assert_eq!(result.affected_count, 0, "连续失败超限应跳过");
}

#[tokio::test]
async fn test_run_compact_micro_resets_failures() {
    let mut t = MessageTranscript::new();
    // 构造足够多的轮次
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 2u32;
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
    assert_eq!(failures, 0, "成功后应重置失败计数");
}

#[tokio::test]
async fn test_run_compact_rerun_clears_stale_excluded_flags() {
    // 连续失败超限后 skip 而非强制重跑——已在上层消除 stale flag 清除逻辑
    // （v2 lifecycle commit 模式下，Full 以原子事务覆盖全部标记，不再分步清除）
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("question 1"));
    let id2 = t.append(make_ai("answer 1"));

    // 模拟上轮失败留下的标记
    t.set_excluded(id1, true);
    t.set_excluded(id2, true);
    assert!(t.flags(id1).excluded);
    assert!(t.flags(id2).excluded);

    let config = CompactConfig::default();
    let mut failures = config.max_consecutive_failures; // 已达到上限
    let pressure = pressure_from_budget(0.5);
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

    assert_eq!(result.strategy, CompactStrategy::Skip, "连续失败超限应跳过");
    assert!(t.flags(id1).excluded, "excluded 标记不再在 skip 时自动清除");
    assert!(t.flags(id2).excluded, "excluded 标记不再在 skip 时自动清除");
}

#[tokio::test]
async fn test_run_compact_first_run_does_not_clear_flags() {
    // 首次运行（failures=0）不应触碰 flags
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("q"));
    t.append(make_ai("a"));

    // 手动预设置 excluded（模拟其他来源的标记）
    t.set_excluded(id1, true);

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.5);
    let _ = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        true,
        &mut failures,
        "/tmp",
    )
    .await;

    // 首次运行不应清除（虽然 full_compact_inner 失败，但清除逻辑未触发）
    assert!(t.flags(id1).excluded, "首次运行不应清除 excluded");
}

#[tokio::test]
async fn test_run_compact_rerun_only_clears_excluded_not_truncated() {
    // 连续失败超限后 skip——不会主动清除任何 flag。
    // v2 lifecycle commit 模式下，Compact 以原子事务管理全部标记，不再针对 excluded/truncated 做分步清除。
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("q"));
    t.append(make_ai("a"));

    // id1 同时有 truncated + excluded
    t.set_truncated(id1, true);
    t.set_excluded(id1, true);
    assert!(t.flags(id1).truncated);
    assert!(t.flags(id1).excluded);

    let config = CompactConfig::default();
    let mut failures = config.max_consecutive_failures;
    let pressure = pressure_from_budget(0.5);
    let _ = run_compact(
        &mut t,
        None,
        &config,
        &pressure,
        true,
        &mut failures,
        "/tmp",
    )
    .await;

    // skip 后所有标记原样保留
    assert!(t.flags(id1).excluded, "skip 不清除 excluded");
    assert!(t.flags(id1).truncated, "skip 不清除 truncated");
}

// ── Task 6: 端到端 economy 字段 + projection 测试 ────────────────────────

use crate::agent::compact_v2::planner::{plan_micro, FullEscalationReason};

#[tokio::test]
async fn test_compact_result_economy_fields_populated() {
    // Micro Compact 后 CompactResult 应包含非零 economy 字段
    // 使用足够长的内容保证 token 估算 > 0
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2000);
    for i in 0..8 {
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
    assert!(result.affected_count > 0, "Micro 应标记消息");
    assert!(
        result.estimated_tokens_saved > 0,
        "Micro 应估算节省 token > 0，实际: {}",
        result.estimated_tokens_saved
    );
    assert_eq!(result.full_escalation_reason, None, "Micro 不应有升级原因");
    assert!(result.summary.is_none(), "Micro 不应有摘要");
}

#[tokio::test]
async fn test_full_compact_escalation_reason_preserved() {
    // Full Compact（force=true）失败降级时 escalation_reason 应保留
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));
    t.append(make_ai("answer"));

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let pressure = pressure_from_budget(0.5);
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
        result.full_escalation_reason,
        Some(FullEscalationReason::ManualForce),
        "force=true 应记录 ManualForce 升级原因"
    );
}

#[test]
fn test_compact_plan_empty_no_projection_side_effects() {
    // 空 transcript 的 plan_micro 应返回空 plan，has_changes() == false
    let mut t = MessageTranscript::new();
    // 仅一条 human 消息，不足 stale_steps=4
    t.append(make_human("hello"));

    let config = CompactConfig::default();
    let plan = plan_micro(&t, &config, false);

    assert!(!plan.has_changes(), "单消息 transcript 的 plan 应为空");
    assert_eq!(plan.estimated_tokens_saved, 0, "空 plan 不应有 token 节省");
    assert!(plan.actions.is_empty(), "空 plan 不应有 actions");
}

#[test]
fn test_compact_plan_has_changes_with_enough_rounds() {
    // 足够的轮次应产生非空 plan
    // 与 planner_test.rs 中的 test_token_estimation_produces_nonzero_savings 对齐
    let mut t = MessageTranscript::new();
    let long_output = "x".repeat(2000);
    for i in 0..8 {
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
        ..Default::default()
    };
    let plan = plan_micro(&t, &config, false);

    assert!(plan.has_changes(), "多轮 transcript 应有非空 plan");
    assert!(plan.estimated_tokens_saved > 0, "应估算非零 token 节省");
    assert!(!plan.actions.is_empty(), "应有至少一个 action");
}

// ── Ancestor 恢复测试：compact 后 ancestor flags 正确保留 ─────────────────

#[tokio::test]
async fn test_run_compact_preserves_ancestor_flags_after_micro() {
    // compact 后通过 selector 恢复路径（visible_messages）确认 ancestor flags 不变
    let ancestor1 = make_human("ancestor question");
    let ancestor2 = make_ai("ancestor answer");
    let mut t = MessageTranscript::new().with_ancestor(vec![ancestor1, ancestor2]);

    // 添加自有消息构成足够轮次触发 micro
    for i in 0..8 {
        t.append(make_human(&format!("own q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let entries = t.entries();
    let ancestor_id_0 = entries[0].message.id();
    let ancestor_id_1 = entries[1].message.id();
    // 确认 compact 前 ancestor 无任何标记
    assert!(!t.flags(ancestor_id_0).truncated);
    assert!(!t.flags(ancestor_id_0).excluded);
    assert!(!t.flags(ancestor_id_1).truncated);
    assert!(!t.flags(ancestor_id_1).excluded);

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

    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert!(result.affected_count > 0, "Micro 应标记自有消息");

    // 验证：compact 后 ancestor flags 不得改变
    assert!(
        !t.flags(ancestor_id_0).truncated,
        "ancestor[0] 不得被 truncated"
    );
    assert!(
        !t.flags(ancestor_id_0).excluded,
        "ancestor[0] 不得被 excluded"
    );
    assert!(
        !t.flags(ancestor_id_1).truncated,
        "ancestor[1] 不得被 truncated"
    );
    assert!(
        !t.flags(ancestor_id_1).excluded,
        "ancestor[1] 不得被 excluded"
    );

    // 模拟 ACP selector 恢复路径：visible_messages() 应包含 ancestor 消息
    let visible = t.visible_messages();
    let visible_ids: Vec<_> = visible.iter().map(|m| m.id()).collect();
    assert!(
        visible_ids.contains(&ancestor_id_0),
        "visible_messages 应包含 ancestor[0]"
    );
    assert!(
        visible_ids.contains(&ancestor_id_1),
        "visible_messages 应包含 ancestor[1]"
    );

    // Micro compact 使用 truncated 标记（不影响可见性），自有消息中应有 truncated
    let entries_after = t.entries();
    let any_own_truncated = entries_after
        .iter()
        .skip(2) // 跳过 2 条 ancestor
        .any(|e| t.flags(e.message.id()).truncated);
    assert!(
        any_own_truncated,
        "compact 后至少一条自有消息应有 truncated 标记"
    );
}
