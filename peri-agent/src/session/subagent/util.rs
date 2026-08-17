use std::sync::Arc;

use crate::agent::react::AgentOutput;
use crate::messages::BaseMessage;
use crate::session::Session;

// ─── 工具函数（自 tool/mod.rs / mod.rs 迁移） ──────────────────────────────

/// 从 session transcript 提取最后一条非空 AI 消息文本（P1-11: 各执行路径共用）。
pub fn extract_last_ai_text(session: &Arc<Session>) -> String {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// 从 session transcript 统计 subagent 实际执行的工具调用次数。
///
/// 遍历 `visible_messages()` 中所有 `BaseMessage::Tool` 条目——每条对应一次
/// 工具执行（含成功和失败）。
pub fn count_tool_calls_from_session(session: &Arc<Session>) -> usize {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .filter(|m| matches!(m, BaseMessage::Tool { .. }))
        .count()
}

/// Format sub-agent execution result as a summary string returned to the parent agent.
pub fn format_subagent_result(output: &AgentOutput) -> String {
    if output.tool_calls.is_empty() {
        return output.text.clone();
    }

    let mut tool_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (call, _) in &output.tool_calls {
        *tool_counts.entry(call.name.as_str()).or_insert(0) += 1;
    }

    let mut tools: Vec<_> = tool_counts.into_iter().collect();
    tools.sort_by_key(|b| std::cmp::Reverse(b.1));

    let tool_summary = tools
        .into_iter()
        .map(|(name, count)| format!("{} {} times", name, count))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "[Sub-agent executed {} tool calls: {}]\n\n{}",
        output.tool_calls.len(),
        tool_summary,
        output.text
    )
}
