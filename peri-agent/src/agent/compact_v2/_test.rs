//! run_compact 集成测试
//!
//! 测试顶层入口 run_compact 在不同条件下的行为。

use crate::agent::compact_v2::config::CompactConfig;
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

// ── run_compact 集成测试（无 LLM） ────────────────────────────────────────

#[tokio::test]
async fn test_run_compact_low_budget_skips() {
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));
    t.append(make_ai("answer"));

    let config = CompactConfig::default();
    let mut failures = 0u32;
    let result = run_compact(&mut t, None, &config, 0.5, false, &mut failures, "/tmp").await;
    assert_eq!(result.affected_count, 0, "低预算应跳过");
}

#[tokio::test]
async fn test_run_compact_micro_threshold() {
    let mut t = MessageTranscript::new();
    // 构造足够多的轮次以触发 micro
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // budget = 0.75 → micro 范围
    let result = run_compact(&mut t, None, &config, 0.75, false, &mut failures, "/tmp").await;
    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert!(result.affected_count > 0, "应有消息被标 truncated");
}

#[tokio::test]
async fn test_run_compact_full_no_llm_fails_gracefully() {
    let mut t = MessageTranscript::new();
    t.append(make_human("question"));
    t.append(make_ai("answer"));

    let config = CompactConfig::default();
    let mut failures = 0u32;
    // force=true 但无 LLM → 失败降级
    let result = run_compact(&mut t, None, &config, 0.5, true, &mut failures, "/tmp").await;
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
    let result = run_compact(&mut t, None, &config, 0.95, true, &mut failures, "/tmp").await;
    assert_eq!(result.affected_count, 0, "连续失败超限应跳过");
}

#[tokio::test]
async fn test_run_compact_micro_resets_failures() {
    let mut t = MessageTranscript::new();
    // 构造足够多的轮次
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let mut failures = 2u32;
    let result = run_compact(&mut t, None, &config, 0.75, false, &mut failures, "/tmp").await;
    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert_eq!(failures, 0, "成功后应重置失败计数");
}

#[tokio::test]
async fn test_run_compact_rerun_clears_stale_excluded_flags() {
    // 上轮 Full Compact 失败留下 excluded 标记，本轮重跑前应清除
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("question 1"));
    let id2 = t.append(make_ai("answer 1"));

    // 模拟上轮失败：手动标记 excluded
    t.set_excluded(id1, true);
    t.set_excluded(id2, true);
    assert!(t.flags(id1).excluded);
    assert!(t.flags(id2).excluded);

    let config = CompactConfig::default();
    let mut failures = 1u32; // 上轮失败一次
                             // force=true 触发 Full，但 consecutive_failures>0，应先清除 excluded
                             // 然后无 LLM 调用 full_compact_inner 会失败（但清除已发生）
    let result = run_compact(&mut t, None, &config, 0.5, true, &mut failures, "/tmp").await;

    assert_eq!(result.strategy, CompactStrategy::Full);
    assert!(!t.flags(id1).excluded, "上轮 excluded 标记应被清除");
    assert!(!t.flags(id2).excluded, "上轮 excluded 标记应被清除");
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
    let _ = run_compact(&mut t, None, &config, 0.5, true, &mut failures, "/tmp").await;

    // 首次运行不应清除（虽然 full_compact_inner 失败，但清除逻辑未触发）
    assert!(t.flags(id1).excluded, "首次运行不应清除 excluded");
}

#[tokio::test]
async fn test_run_compact_rerun_only_clears_excluded_not_truncated() {
    // 重跑时应清除 excluded，但保留 truncated（属 Micro Compact）
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("q"));
    t.append(make_ai("a"));

    // id1 同时有 truncated + excluded
    t.set_truncated(id1, true);
    t.set_excluded(id1, true);
    assert!(t.flags(id1).truncated);
    assert!(t.flags(id1).excluded);

    let config = CompactConfig::default();
    let mut failures = 1u32;
    let _ = run_compact(&mut t, None, &config, 0.5, true, &mut failures, "/tmp").await;

    // 重跑后 excluded 被清除，但 truncated 保留
    assert!(!t.flags(id1).excluded, "重跑应清除 excluded");
    assert!(
        t.flags(id1).truncated,
        "重跑不应清除 truncated（属 Micro Compact 状态）"
    );
}
