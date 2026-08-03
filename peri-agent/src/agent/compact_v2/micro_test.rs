//! Tests for micro

use super::*;
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::projection::PROJECTION_POLICY_VERSION;
use crate::agent::compact_v2::{determine_compact_action, CompactAction};
use crate::messages::{BaseMessage, ContentBlock, MessageContent};
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

// ── Micro Compact 测试 ─────────────────────────────────────────────────────

#[test]
fn test_micro_compact_empty_transcript() {
    let mut t = MessageTranscript::new();
    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0);
}

#[test]
fn test_micro_compact_all_within_stale_window() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));
    let _id = t.append(make_tool_result("call_1", "large output here"));
    let config = CompactConfig::default();
    // 只有 1 轮，stale_steps 默认 3，全部在窗口内 → 不截断
    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0);
}

#[test]
fn test_micro_compact_marks_old_tool_results() {
    let mut t = MessageTranscript::new();
    for i in 0..6 {
        t.append(make_human(&format!("question {}", i)));
        let ai_id = format!("call_{}", i);
        t.append(make_ai_with_tool("thinking...", "Bash", &ai_id));
        t.append(make_tool_result(&ai_id, &format!("output {}", i)));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    // 第 0 轮的 tool_use + tool_result 都应被截断
    assert!(
        affected >= 2,
        "tool_use + tool_result 应被截断，实际: {}",
        affected
    );
}

#[test]
fn test_micro_compact_skips_error_tool_results() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));
    t.append(make_ai_with_tool("thinking...", "Bash", "call_1"));
    t.append(make_tool_result("call_1", "error output"));

    // 只有 1 轮，stale_steps=3 → 所有消息在窗口内，affected=0
    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "只有 1 轮，全在 stale 窗口内");
}

#[test]
fn test_micro_compact_respects_ancestor_boundary() {
    let ancestor = make_human("ancestor message");
    let mut t = MessageTranscript::new().with_ancestor(vec![ancestor]);
    t.append(make_human("own message"));

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "ancestor 消息不应被截断");
    // ancestor 消息不应被标 truncated
    let ancestor_id = t.entries()[0].message.id();
    assert!(!t.flags(ancestor_id).truncated);
}

#[test]
fn test_micro_compact_no_duplicate_truncation() {
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let first = micro_compact(&mut t, &config);
    let second = micro_compact(&mut t, &config);
    assert_eq!(second, 0, "重复调用不应增加标记");
    assert!(first > 0, "首次调用应有消息被标记");
}

#[test]
fn test_micro_compact_truncated_still_visible() {
    let mut t = MessageTranscript::new();
    let id = t.append(make_human("some message"));
    t.set_truncated(id, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1, "truncated 消息仍然可见");
    assert_eq!(visible[0].id(), id);
}

#[test]
fn test_micro_compact_truncates_tool_use_arguments() {
    let mut t = MessageTranscript::new();
    // 构造足够多轮次，使第 0 轮的 Ai 消息被截断
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        // Write 工具的 tool_use 有大量 arguments（如 file content）
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("I'll write the file"),
            vec![crate::messages::ToolCallRequest::new(
                format!("call_{}", i),
                "Write",
                serde_json::json!({"file_path": "/tmp/test.txt", "content": "x".repeat(501)}),
            )],
        ));
        t.append(make_tool_result(&format!("call_{}", i), "Wrote file"));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    // 第 0-1 轮的 Ai (tool_use) + Tool (tool_result) 都应被截断
    assert!(
        affected >= 2,
        "tool_use + tool_result 应被截断，实际: {}",
        affected
    );

    // 确认第 0 条 Ai 消息被标 truncated（tool_use input）
    let ai_id = t.entries()[1].message.id();
    assert!(
        t.flags(ai_id).truncated,
        "Ai 消息（含 Write tool_use arguments）应被 truncated"
    );
}

#[test]
fn test_micro_compact_respects_blacklist() {
    // 将 Bash 加入黑名单——Bash tool_use 和 tool_result 都不应截断
    let config = CompactConfig {
        micro_excluded_tools: vec!["Bash".to_string()],
        ..Default::default()
    };

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("bash_{}", i)));
        t.append(make_tool_result(
            &format!("bash_{}", i),
            &format!("bash output {}", i),
        ));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "Bash 在黑名单中，不应被截断");
}

#[test]
fn test_micro_compact_blacklist_case_insensitive() {
    let config = CompactConfig {
        micro_excluded_tools: vec!["bash".to_string()],
        ..Default::default()
    };

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "黑名单应大小写无关");
}

#[test]
fn test_micro_compact_low_affected_does_not_break_transcript() {
    let mut t = MessageTranscript::new();
    // 只有 3 轮，第 0 轮在 stale 窗口外，但只有 Human+Ai+Tool 共 3 条 → affected=3 < 5
    for i in 0..3 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let affected = micro_compact(&mut t, &config);
    assert!(affected > 0, "应有消息被截断");
    assert!(affected < 5, "affected 应 < 5（模拟 Micro 无效场景）");

    // 验证 truncated 消息仍然可见（后续 Full 可读完整内容生成摘要）
    let visible = t.visible_messages();
    assert_eq!(visible.len(), 9, "truncated 消息仍应全部可见");
}

#[test]
fn test_micro_compact_ask_user_question_preserved_by_default() {
    // 默认黑名单包含 AskUserQuestion，其 tool_result 和纯 AskUserQuestion 的 Ai 消息应被保留
    let config = CompactConfig::default();

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        // 每个 Ai 消息只含 AskUserQuestion 一个 tool_call
        t.append(make_ai_with_tool(
            "let me ask",
            "AskUserQuestion",
            &format!("au_{}", i),
        ));
        t.append(make_tool_result(
            &format!("au_{}", i),
            &format!("user answer {}", i),
        ));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(
        affected, 0,
        "AskUserQuestion 在黑名单中，所有消息不应被截断"
    );
}

// ── 特征化测试：现有行为基线 ───────────────────────────────────────────────

#[test]
fn test_low_budget_skips_micro() {
    // budget < 0.75 → determine_compact_action 返回 Skip
    let config = CompactConfig::default();
    assert_eq!(determine_compact_action(0.50, &config), CompactAction::Skip);
    assert_eq!(determine_compact_action(0.74, &config), CompactAction::Skip);
    assert_eq!(
        determine_compact_action(0.74999, &config),
        CompactAction::Skip,
        "边界值：budget < 0.75 应 Skip"
    );
}

#[test]
fn test_protected_tools_not_selected() {
    // 默认黑名单中的工具（AskUserQuestion/goal/TodoWrite）不被选中截断
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        // 混用受保护和不受保护的工具
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("using tools"),
            vec![
                crate::messages::ToolCallRequest::new(
                    format!("call_{}", i),
                    "goal",
                    serde_json::json!({"content": "x".repeat(501)}),
                ),
                crate::messages::ToolCallRequest::new(
                    format!("call_bash_{}", i),
                    "Bash",
                    serde_json::json!({"content": "x".repeat(501)}),
                ),
            ],
        ));
        t.append(make_tool_result(&format!("call_{}", i), "goal ok"));
        t.append(make_tool_result(&format!("call_bash_{}", i), "bash ok"));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    // 受保护的 goal tool_call 存在 → Ai 消息仍被截断（因为 Bash 也在其中）
    // 但 goal 的 tool_result 不应被截断
    assert!(affected > 0, "Bash tool 应被截断");

    // 验证 goal 的 tool_result 未被截断
    for entry in t.entries() {
        if let BaseMessage::Tool { tool_call_id, .. } = &entry.message {
            if tool_call_id.starts_with("call_") && !tool_call_id.contains("bash") {
                assert!(
                    !t.flags(entry.message.id()).truncated,
                    "受保护工具 goal 的 tool_result 不应被截断"
                );
            }
        }
    }
}

#[test]
fn test_error_tool_result_not_selected() {
    // 错误 ToolResult（is_error=true）不被截断，保留诊断信息
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool(
            "run command",
            "Bash",
            &format!("bash_{}", i),
        ));
        // 错误工具结果
        t.append(BaseMessage::tool_error(
            format!("bash_{}", i),
            MessageContent::text(format!("error output {}", i)),
        ));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    // 错误 tool_result 不被截断 → 没有 Tool 消息会被标记
    // 但 Ai 消息（含 Bash tool_use）仍可能被截断
    // 验证所有错误 tool_result 未被截断
    for entry in t.entries() {
        if let BaseMessage::Tool { is_error, .. } = &entry.message {
            if *is_error {
                assert!(
                    !t.flags(entry.message.id()).truncated,
                    "错误 ToolResult 不应被截断"
                );
            }
        }
    }
    assert!(affected > 0, "至少 Ai 消息应被截断");
}

#[test]
fn test_ancestor_never_selected() {
    // ancestor_len 之前的消息（祖先区域）永不被标记截断
    let mut t = MessageTranscript::new().with_ancestor(vec![
        make_human("ancestor question"),
        make_ai_with_tool("ancestor tool call", "Bash", "acall_0"),
        make_tool_result("acall_0", "ancestor output"),
    ]);
    // 祖先区域有 3 条，ancestor_len = 3

    // 追加足够多的自有消息让第 0 轮进入 stale 窗口
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert!(affected > 0, "自有消息应被截断");

    // 祖先区域消息不应有任何标记
    for entry in t.entries().iter().take(3) {
        assert!(!t.flags(entry.message.id()).truncated, "祖先消息不应被截断");
    }
}

#[test]
fn test_micro_compact_todo_write_preserved_by_default() {
    // TodoWrite 也在默认黑名单中
    let config = CompactConfig::default();

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "TodoWrite", &format!("td_{}", i)));
        t.append(make_tool_result(&format!("td_{}", i), "todo updated"));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "TodoWrite 在黑名单中，不应被截断");
}

#[test]
fn test_protected_by_retention_map_not_selected() {
    // retention_map 保护生效：Preserve 工具不被截断
    use crate::tools::ContextRetention;
    use std::collections::HashMap;

    let mut retention_map = HashMap::new();
    retention_map.insert("mycustomtool".to_string(), ContextRetention::Preserve);

    let config = CompactConfig {
        tool_retention_map: retention_map,
        micro_excluded_tools: vec![], // 黑名单为空，完全依赖 retention_map
        ..Default::default()
    };

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool(
            "using custom tool",
            "MyCustomTool",
            &format!("ct_{}", i),
        ));
        t.append(make_tool_result(
            &format!("ct_{}", i),
            &format!("custom output {}", i),
        ));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "retention_map 中 Preserve 工具不应被截断");
}

#[test]
fn test_micro_compact_short_param_tool_call_not_compacted() {
    // 回归验证：普通短参数工具调用（<500 字符）不再产生占位压缩 action。
    // 历史上短参数会走整条兜底压缩（fields 空 → `{"_compact_note": ...}` 占位），
    // LLM 模仿输出占位导致真实工具执行失败，兜底已移除。
    // 无超长字段 + 无超长结果 → plan 空 → Micro Compact Skip（无可压缩内容）。
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        let ai = BaseMessage::ai_with_tool_calls(
            MessageContent::text(format!("thinking {}", i)),
            vec![crate::messages::ToolCallRequest::new(
                format!("sc_{}", i),
                "Read",
                serde_json::json!({"path": "/tmp/short.txt"}),
            )],
        );
        t.append(ai);
        t.append(BaseMessage::tool_result(
            format!("sc_{}", i),
            MessageContent::text("short result"),
        ));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert_eq!(
        affected, 0,
        "短参数工具调用不再产生压缩 action（affected={affected}）"
    );

    // 无任何 Ai 消息被标记 truncated
    let flagged_ai = t
        .entries()
        .iter()
        .filter(|e| matches!(&e.message, BaseMessage::Ai { .. }))
        .filter(|e| t.flags(e.message.id()).truncated)
        .count();
    assert_eq!(flagged_ai, 0, "短参数 tool_use 消息不应被标记 truncated");
}

// ── 工厂函数：供后续测试复用 ──────────────────────────────────────────────

#[allow(dead_code)]
fn make_text_tool_result(id: &str, name: &str, output: &str) -> BaseMessage {
    // name 参数供上层语义匹配，BaseMessage::Tool 内部不存储工具名
    let _ = name;
    BaseMessage::tool_result(id.to_string(), MessageContent::text(output.to_string()))
}

#[allow(dead_code)]
fn make_blocks_message(blocks: Vec<ContentBlock>) -> BaseMessage {
    BaseMessage::ai(MessageContent::blocks(blocks))
}

#[allow(dead_code)]
fn make_image_block() -> ContentBlock {
    ContentBlock::Image {
        source: crate::messages::ImageSource::Base64 {
            media_type: "image/png".to_string(),
            data: "fake_base64".to_string(),
        },
    }
}

#[allow(dead_code)]
fn make_document_block() -> ContentBlock {
    ContentBlock::Document {
        source: crate::messages::DocumentSource::Base64 {
            media_type: "text/plain".to_string(),
            data: "fake_base64".to_string(),
        },
        title: None,
    }
}

#[allow(dead_code)]
fn make_ai_with_tool_calls_vec(tool_calls: Vec<crate::messages::ToolCallRequest>) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(MessageContent::text("".to_string()), tool_calls)
}

// ─── 持久化 projection directive 测试 ───────────────────────────────────────

#[test]
fn test_micro_compact_writes_projection_directives() {
    // 验证 micro_compact 不仅标记 truncated，还将完整的投影指令写入 flags.projection
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("question {}", i)));
        t.append(make_ai_with_tool(
            "thinking...",
            "Bash",
            &format!("call_{}", i),
        ));
        t.append(make_tool_result(
            &format!("call_{}", i),
            &format!("output {}", i),
        ));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert!(affected > 0, "应有消息被标记");

    // 检查受影响消息的 flags.projection 非空
    let mut found_directive = false;
    for entry in t.entries() {
        let flags = t.flags(entry.message.id());
        if flags.truncated {
            assert!(
                flags.projection.is_some(),
                "truncated 消息应含 projection directive，msg_id={:?}",
                entry.message.id()
            );
            let directive = flags.projection.as_ref().unwrap();
            assert_eq!(
                directive.policy_version, PROJECTION_POLICY_VERSION,
                "policy_version 应为当前版本"
            );
            assert!(!directive.entries.is_empty(), "directive entries 不应为空");
            found_directive = true;
        }
    }
    assert!(found_directive, "至少一条消息应含 projection directive");
}

#[test]
fn test_micro_compact_projection_entries_match_plan_actions() {
    // 验证 directive entries 与 plan.plan_micro 的输出一致
    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("tool", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    // 先调用 plan_micro 获取预期的 actions
    let plan_without_skip = crate::agent::compact_v2::planner::plan_micro(&t, &config, false);
    // micro_compact 内部用 skip_existing_truncated=true
    micro_compact(&mut t, &config);

    // 收集所有 directive entries (owned)
    let mut directive_entries: Vec<crate::agent::compact_v2::projection::ProjectionActionEntry> =
        Vec::new();
    for entry in t.entries() {
        let flags = t.flags(entry.message.id());
        if let Some(ref directive) = flags.projection {
            directive_entries.extend(directive.entries.iter().cloned());
        }
    }

    // 验证数量：plan（skip_truncated=false）应 >= directive entries（skip_truncated=true）
    assert!(
        plan_without_skip.actions.len() >= directive_entries.len(),
        "plan_micro 的 actions({}) 不应少于 directive entries({})",
        plan_without_skip.actions.len(),
        directive_entries.len()
    );

    // 验证每条 directive entry 都对应 plan 中的 action（message_id + target 匹配）
    for entry in &directive_entries {
        let found = plan_without_skip
            .actions
            .iter()
            .any(|a| a.message_id == entry.message_id && a.target == entry.target);
        assert!(
            found,
            "directive entry 应能在 plan actions 中找到对应项: {:?}",
            entry.target
        );
    }
}
