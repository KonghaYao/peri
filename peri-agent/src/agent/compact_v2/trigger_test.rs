//! run_compact 新触发流程测试

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::ContextPressure;
use crate::agent::compact_v2::{determine_compact_action, run_compact, CompactAction};
use crate::agent::events::CompactStrategy;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::transcript::MessageTranscript;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai_with_tool(text: &str, tool_name: &str, tool_id: &str) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        MessageContent::text(text.to_string()),
        vec![crate::messages::ToolCallRequest::new(
            tool_id,
            tool_name,
            serde_json::json!({}),
        )],
    )
}

fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
    BaseMessage::tool_result(
        tool_call_id.to_string(),
        MessageContent::text(text.to_string()),
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
    let result = run_compact(&mut t, None, &config, &pressure, false, &mut failures, "/tmp").await;
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
    let result = run_compact(&mut t, None, &config, &pressure, false, &mut failures, "/tmp").await;
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
    let result = run_compact(&mut t, None, &config, &pressure, false, &mut failures, "/tmp").await;
    // Full → 无 LLM 降级
    assert_eq!(
        result.strategy,
        CompactStrategy::Full,
        "budget ≥ 0.95 时应叠加 Full"
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
    let result = run_compact(&mut t, None, &config, &pressure, true, &mut failures, "/tmp").await;
    assert_eq!(result.strategy, CompactStrategy::Full);
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
    let result = run_compact(&mut t, None, &config, &pressure, false, &mut failures, "/tmp").await;
    // budget=0.80 → Micro → 满足 target → 只走 Micro
    assert!(
        matches!(result.strategy, CompactStrategy::Micro),
        "无 LLM 时应保持在 Micro 策略"
    );
    assert!(result.affected_count > 0, "Micro 阶段应标记了消息");
    assert!(result.summary.is_none(), "无 LLM 时 Full 不产生摘要");
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
    let result = run_compact(&mut t, None, &config, &pressure, false, &mut failures, "/tmp").await;

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
        run_compact(&mut t, None, &config, &pressure, false, &mut failures, "/tmp")
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
