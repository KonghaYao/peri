//! Tests for planner — 计划器单元测试

use super::planner::plan_micro;
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::projection::MicroCompactPlan;
use crate::session::transcript::MessageTranscript;

#[test]
fn test_context_pressure_target_tokens() {
    use super::planner::ContextPressure;
    let p = ContextPressure {
        estimated_tokens: 100_000,
        context_window: 200_000,
        output_reserve: 8000,
        predicted_tool_growth: 4000,
        safety_buffer: 2000,
        cache_hit_rate: 0.5,
    };
    assert_eq!(p.target_tokens(), 186_000);
    assert_eq!(p.target_reclaim_tokens(), 0);
}

#[test]
fn test_micro_compact_plan_meets_target() {
    let plan = MicroCompactPlan {
        policy_version: 1,
        target_reclaim_tokens: 5000,
        actions: vec![],
        estimated_before_tokens: 10000,
        estimated_after_tokens: 4000,
        estimated_tokens_saved: 6000,
    };
    assert!(plan.meets_target());
    assert!(plan.has_changes());
}

#[test]
fn test_micro_compact_plan_no_changes() {
    let plan = MicroCompactPlan {
        policy_version: 1,
        target_reclaim_tokens: 5000,
        actions: vec![],
        estimated_before_tokens: 10000,
        estimated_after_tokens: 10000,
        estimated_tokens_saved: 0,
    };
    assert!(!plan.has_changes());
}

#[test]
fn test_plan_micro_empty_transcript() {
    let t = MessageTranscript::new();
    let config = CompactConfig::default();
    let plan = plan_micro(&t, &config, false);
    assert_eq!(plan.actions.len(), 0);
    assert!(!plan.has_changes());
}

// ── Token 估算测试 ────────────────────────────────────────────────────────

#[test]
fn test_token_estimation_produces_nonzero_savings() {
    // 有 action 时估算非零
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};

    let mut t = MessageTranscript::new();
    // 添加多轮对话 + tool calls 触发 Micro action
    for i in 0..8 {
        t.append(BaseMessage::human(MessageContent::text(format!(
            "question {}",
            i
        ))));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text(format!("thinking {}", i)),
            vec![ToolCallRequest::new(
                &format!("call_{}", i),
                "Bash",
                serde_json::json!({"cmd": format!("echo {}", i)}),
            )],
        ));
        let long_output = "x".repeat(2000); // 2000 chars 输出
        t.append(BaseMessage::tool_result(
            format!("call_{}", i),
            MessageContent::text(long_output),
        ));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..CompactConfig::default()
    };
    let plan = plan_micro(&t, &config, false);
    assert!(!plan.actions.is_empty(), "应有 actions");
    assert!(
        plan.estimated_tokens_saved > 0,
        "有 action 时估算节省应 > 0"
    );
    assert!(
        plan.estimated_before_tokens > plan.estimated_after_tokens,
        "投影后应减少"
    );
}

#[test]
fn test_token_estimation_no_actions_saves_zero() {
    // 无 action 时节省=0
    let mut t = MessageTranscript::new();
    // 只有 Human 消息，不会触发 Micro action
    for i in 0..3 {
        t.append(make_human(&format!("msg {}", i)));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 0, // 所有轮次都 stale
        ..CompactConfig::default()
    };
    let plan = plan_micro(&t, &config, false);
    // Human 消息不生成 tool exchange → actions 为 0
    assert_eq!(plan.actions.len(), 0, "Human-only 消息不应有 actions");
    assert_eq!(plan.estimated_tokens_saved, 0, "无 action 时节省应为 0");
}

#[test]
fn test_context_pressure_target_reclaim_within_window() {
    // 确认回收目标计算正确
    use super::planner::ContextPressure;

    // 场景：180k / 200k → 已用很多，需要回收
    let p = ContextPressure {
        estimated_tokens: 180_000,
        context_window: 200_000,
        output_reserve: 8000,
        predicted_tool_growth: 4000,
        safety_buffer: 5000,
        cache_hit_rate: 0.0,
    };
    // target_tokens = 200000 - 8000 - 4000 - 5000 = 183000
    assert_eq!(p.target_tokens(), 183_000);
    // 180k < 183k → reclaim = 0
    assert_eq!(p.target_reclaim_tokens(), 0);

    // 场景：195k / 200k → 需要回收
    let p2 = ContextPressure {
        estimated_tokens: 195_000,
        context_window: 200_000,
        output_reserve: 8000,
        predicted_tool_growth: 4000,
        safety_buffer: 5000,
        cache_hit_rate: 0.0,
    };
    // target_tokens = 200000 - 8000 - 4000 - 5000 = 183000
    // reclaim = 195000 - 183000 = 12000
    assert_eq!(p2.target_reclaim_tokens(), 12_000);
}

fn make_human(text: &str) -> crate::messages::BaseMessage {
    use crate::messages::{BaseMessage, MessageContent};
    BaseMessage::human(MessageContent::text(text.to_string()))
}

// ── Retention Metadata 测试 ──────────────────────────────────────────────

#[test]
fn test_retention_map_preserve_blocks_compact() {
    // Preserve 工具不产生 action
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};
    use crate::tools::ContextRetention;
    use std::collections::HashMap;

    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("thinking".to_string()),
            vec![ToolCallRequest::new(
                &format!("call_{}", i),
                "MyTool",
                serde_json::json!({}),
            )],
        ));
        t.append(BaseMessage::tool_result(
            format!("call_{}", i),
            MessageContent::text("output"),
        ));
    }

    let mut retention_map = HashMap::new();
    retention_map.insert("mytool".to_string(), ContextRetention::Preserve);

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        tool_retention_map: retention_map,
        ..CompactConfig::default()
    };

    let plan = plan_micro(&t, &config, false);
    assert_eq!(
        plan.actions.len(),
        0,
        "Preserve 工具的 tool exchange 不应产生 action"
    );
}

#[test]
fn test_retention_map_recomputable_allows_compact() {
    // Recomputable 工具产生 action
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};
    use crate::tools::ContextRetention;
    use std::collections::HashMap;

    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("thinking".to_string()),
            vec![ToolCallRequest::new(
                &format!("call_{}", i),
                "ReadTool",
                serde_json::json!({}),
            )],
        ));
        t.append(BaseMessage::tool_result(
            format!("call_{}", i),
            MessageContent::text("output"),
        ));
    }

    let mut retention_map = HashMap::new();
    retention_map.insert("readtool".to_string(), ContextRetention::Recomputable);

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        tool_retention_map: retention_map,
        ..CompactConfig::default()
    };

    let plan = plan_micro(&t, &config, false);
    assert!(!plan.actions.is_empty(), "Recomputable 工具应产生 action");
}

#[test]
fn test_fallback_to_excluded_tools_when_map_empty() {
    // 空 retention_map 时使用旧黑名单
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};

    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("thinking".to_string()),
            vec![ToolCallRequest::new(
                &format!("call_{}", i),
                "AskUserQuestion",
                serde_json::json!({}),
            )],
        ));
        t.append(BaseMessage::tool_result(
            format!("call_{}", i),
            MessageContent::text("user answer"),
        ));
    }

    // 默认配置：tool_retention_map 为空，AskUserQuestion 在 micro_excluded_tools 中
    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..CompactConfig::default()
    };

    let plan = plan_micro(&t, &config, false);
    assert_eq!(
        plan.actions.len(),
        0,
        "空 retention_map 时应 fallback 到 micro_excluded_tools 黑名单"
    );
}
