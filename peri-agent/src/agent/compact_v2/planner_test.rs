//! Tests for planner — 计划器单元测试

use super::planner::plan_micro;
use super::ContextPressure;
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::projection::{
    MicroCompactPlan, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};
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
    assert_eq!(p.target_reclaim_tokens(), 4_000, "2% floor 生效");
}

#[test]
fn test_micro_compact_plan_meets_target() {
    let plan = MicroCompactPlan {
        policy_version: 1,
        target_reclaim_tokens: 5000,
        actions: vec![ProjectionActionEntry {
            message_id: crate::messages::MessageId::new(),
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 100,
                keep_tail: 200,
                preserve_recovery_handle: true,
            },
        }],
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
                format!("call_{}", i),
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
    // 180k < 183k 原为 0，现 floor = 4_000
    assert_eq!(p.target_reclaim_tokens(), 4_000);

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
                format!("call_{}", i),
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
                format!("call_{}", i),
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
                format!("call_{}", i),
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

/// 核心回归测试：Reason 阶段（skip=false）应对已 truncated 的消息生成非空 plan
#[test]
fn test_plan_micro_skip_false_includes_truncated_messages() {
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};

    let mut t = MessageTranscript::new();
    // 构造 10 轮对话，每轮包含 Read 工具调用
    for i in 0..10 {
        t.append(make_human(&format!("q {}", i)));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("thinking".to_string()),
            vec![ToolCallRequest::new(
                format!("call_{}", i),
                "Read",
                serde_json::json!({"file_path": format!("/f/{}", i)}),
            )],
        ));
        t.append(BaseMessage::tool_result(
            format!("call_{}", i),
            MessageContent::text(format!(
                "file {} contains a lot of content: {}",
                i,
                "x".repeat(200)
            )),
        ));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 2, // 仅保留最近 2 轮
        ..CompactConfig::default()
    };

    // Step 1: Compact 阶段（skip=true）— 生成 plan 并打 truncated
    let compact_plan = plan_micro(&t, &config, true);
    let action_ids: Vec<_> = compact_plan.actions.iter().map(|a| a.message_id).collect();
    assert!(
        !action_ids.is_empty(),
        "Compact 阶段 (skip=true) 应对未标记消息生成 action"
    );
    for id in &action_ids {
        t.set_truncated(*id, true);
    }

    // Step 2: Reason 阶段（skip=false）— 应对已 truncated 消息生成非空 plan
    let reason_plan = plan_micro(&t, &config, false);
    assert!(
        !reason_plan.actions.is_empty(),
        "Reason 阶段 (skip=false) 应对已 truncated 消息生成投影 action"
    );
    assert!(
        reason_plan.has_changes(),
        "Reason 阶段 plan 应有 has_changes()=true，避免 fallback 到完整原文"
    );
}

/// 对称测试：Compact 阶段（skip=true）应跳过已有 truncated 的消息
#[test]
fn test_plan_micro_skip_true_excludes_already_truncated() {
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};

    let mut t = MessageTranscript::new();
    for i in 0..10 {
        t.append(make_human(&format!("q {}", i)));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("thinking".to_string()),
            vec![ToolCallRequest::new(
                format!("call_{}", i),
                "Read",
                serde_json::json!({"file_path": format!("/f/{}", i)}),
            )],
        ));
        t.append(BaseMessage::tool_result(
            format!("call_{}", i),
            MessageContent::text(format!(
                "file {} contains a lot of content: {}",
                i,
                "x".repeat(200)
            )),
        ));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 2,
        ..CompactConfig::default()
    };

    // 预标记所有消息为 truncated
    let ids: Vec<_> = t.entries().iter().map(|e| e.message.id()).collect();
    for id in &ids {
        t.set_truncated(*id, true);
    }

    // Compact 阶段应跳过所有消息 → plan 为空
    let plan = plan_micro(&t, &config, true);
    assert_eq!(
        plan.actions.len(),
        0,
        "Compact 阶段 (skip=true) 应跳过所有已 truncated 消息"
    );
}

// ── 验证实验：has_changes() 缺陷 ────────────────────────────────────────

/// 【实验 1】has_changes() 对短消息场景返回 true，因为有实际 action
///
/// 修复后 has_changes() = !actions.is_empty()，不依赖 token 估算。
#[test]
fn test_has_changes_returns_true_for_short_messages_with_actions() {
    let entry = ProjectionActionEntry {
        message_id: crate::messages::MessageId::new(),
        target: ProjectionTarget::Message,
        action: ProjectionAction::CompactToolResult {
            keep_head: 100,
            keep_tail: 200,
            preserve_recovery_handle: true,
        },
    };
    let plan = MicroCompactPlan {
        policy_version: 1,
        target_reclaim_tokens: 0,
        actions: std::iter::repeat_n(entry, 10).collect(),
        estimated_before_tokens: 1, // 短消息：chars=5, /4 = 1
        estimated_after_tokens: 12, // projected_chars=50, /4 = 12
        estimated_tokens_saved: 0,  // saturating_sub(1, 12) = 0
    };
    // 修复后：有 10 个 action → has_changes() 应为 true
    assert!(plan.has_changes(), "有 action 就应该有变化");
}

/// 【实验 2】reclaim_target 在 budget 75%-93.5% 区间恒为 0
///
/// 根因：target_tokens = context_window - reserves ≈ 93.5% 窗口
/// reclaim = estimated_tokens.saturating_sub(target_tokens)
/// budget=80% 时 reclaim = 0，阻断了 "Upgrade to Full" 路径。
#[test]
fn test_reclaim_target_zero_for_mid_range_budgets() {
    // 200K 窗口，默认 reserves
    let p_50 = ContextPressure {
        estimated_tokens: 100_000,
        context_window: 200_000,
        output_reserve: 8000,
        predicted_tool_growth: 0,
        safety_buffer: 5000,
        cache_hit_rate: 0.0,
    };
    assert_eq!(
        p_50.target_reclaim_tokens(),
        4_000,
        "50% 时 floor=4K（原为 0）"
    );

    let p_75 = ContextPressure {
        estimated_tokens: 150_000,
        ..p_50
    };
    assert_eq!(
        p_75.target_reclaim_tokens(),
        4_000,
        "75% 时 floor=4K → Full 升级路径可用"
    );

    let p_85 = ContextPressure {
        estimated_tokens: 170_000,
        ..p_50
    };
    assert_eq!(p_85.target_reclaim_tokens(), 4_000, "85% 时 floor=4K");

    let p_95 = ContextPressure {
        estimated_tokens: 190_000,
        ..p_50
    };
    assert!(
        p_95.target_reclaim_tokens() >= 4_000,
        "95% 时 raw=3K 不足 floor → floor=4K"
    );
}

/// 【实验 3】estimate_tokens 的 .max(1) 防止短消息 saved=0
///
/// 修复后 projected_chars = (chars / 3).max(1)，短消息不会膨胀 projected。
#[test]
fn test_estimate_tokens_max_1_for_short_messages() {
    use crate::agent::compact_v2::planner::plan_micro;
    use crate::messages::{BaseMessage, MessageContent, ToolCallRequest};
    let mut t = MessageTranscript::new();
    for i in 0..8 {
        t.append(make_human(&format!("q {}", i)));
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("thinking"),
            vec![ToolCallRequest::new(
                format!("c_{}", i),
                "Bash",
                serde_json::json!({}),
            )],
        ));
        t.append(BaseMessage::tool_result(
            format!("c_{}", i),
            MessageContent::text(format!("out {}", i)),
        ));
    }
    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..CompactConfig::default()
    };
    let plan = plan_micro(&t, &config, true);
    assert!(!plan.actions.is_empty(), "至少有一些 stale 轮次被选中");
    assert!(plan.estimated_tokens_saved > 0, "有 action 时 saved 应 > 0");
}
