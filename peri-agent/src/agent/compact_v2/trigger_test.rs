//! run_compact 新触发流程测试

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::{run_compact, should_micro_compact, CompactAction};
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

// ── should_micro_compact 测试 ──────────────────────────────────────────

#[test]
fn test_should_micro_compact_below_threshold() {
    let config = CompactConfig::default();
    assert_eq!(should_micro_compact(0.50, &config), CompactAction::Skip);
}

#[test]
fn test_should_micro_compact_above_threshold() {
    let config = CompactConfig::default();
    assert_eq!(should_micro_compact(0.80, &config), CompactAction::Micro);
}

// ── run_compact 触发流程测试（无 LLM） ─────────────────────────────────

#[tokio::test]
async fn test_micro_effective_no_full_overlay() {
    // budget = 0.80 → 75% < 80% < 95% → Micro 有效（affected ≥ 5）→ 不叠加 Full
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let result = run_compact(&mut t, None, &config, 0.80, false, &mut failures, "/tmp").await;
    assert_eq!(result.strategy, CompactStrategy::Micro, "应走 Micro");
    assert!(result.affected_count >= 5, "Micro 有效，不应升级 Full");
    assert!(result.summary.is_none(), "Micro 无摘要");
}

#[tokio::test]
async fn test_micro_invalid_upgrades_to_full() {
    // 仅 3 轮 + stale_steps=1 → affected < 5 → Micro 无效 → 升级 Full（无 LLM → 降级）
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
    let result = run_compact(&mut t, None, &config, 0.80, false, &mut failures, "/tmp").await;
    // 无 LLM → Full 降级，strategy 仍为 Full，但 affected_count=0
    assert_eq!(
        result.strategy,
        CompactStrategy::Full,
        "Micro 无效应升级 Full"
    );
    assert_eq!(failures, 1, "Full 无 LLM 应计失败");
}

#[tokio::test]
async fn test_micro_effective_full_overlay() {
    // budget = 0.98 → ≥ 95% → Micro 有效 + 叠加 Full（无 LLM → 降级）
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let result = run_compact(&mut t, None, &config, 0.98, false, &mut failures, "/tmp").await;
    // 叠加 Full → 无 LLM 降级
    assert_eq!(
        result.strategy,
        CompactStrategy::Full,
        "budget ≥ 0.95 应叠加 Full"
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
    let result = run_compact(&mut t, None, &config, 0.50, true, &mut failures, "/tmp").await;
    assert_eq!(result.strategy, CompactStrategy::Full);
    assert_eq!(failures, 1);
}
