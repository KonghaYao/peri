//! Decode standard session updates without publishing UI state.

use crate::kit::acp_types::AcpEventData;
use crate::kit::slash_completion::SlashActionKind;
use crate::kit::slash_projection::{ArgsSchema, SlashCommandEntry, parse_projection_kind};
use crate::truncate::summarize_input;
use serde_json::Value;

#[derive(Default)]
pub(super) struct StreamUpdate {
    pub event: Option<AcpEventData>,
    pub token_count: Option<usize>,
}

pub(super) fn decode_commands(update: &Value) -> Option<Vec<SlashCommandEntry>> {
    let cmds = update.get("availableCommands")?.as_array()?;
    // Phase 4 步骤 2：每条解析 name（= 投影名：Level1 裸名 / Level2 全名）
    // + description + _meta（_meta 优先、meta 兜底先例，与下方
    // is_session_replay 一致）的 periKind / periLevel / periAliases /
    // periCategory / periArgs；缺省回退 kind=Command / level=1 / args=None
    // / aliases=[]（R1）。
    // **单 atom 原子写**：只写 AVAILABLE_SLASH_COMMANDS，消除现状
    // 「AVAILABLE + SKILL_NAMES + MCP_SKILL_NAMES 三 atom 组合时序」
    // 问题（inv03 §4-R1）；kind 直接来自投影，无集合反推。
    let entries: Vec<SlashCommandEntry> = cmds
        .iter()
        .filter_map(|cmd| {
            let fullname = cmd.get("name")?.as_str()?.to_string();
            let description = cmd
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let meta = cmd.get("_meta").or_else(|| cmd.get("meta"));
            let kind = meta
                .and_then(|m| m.get("periKind"))
                .and_then(|v| v.as_str())
                .and_then(parse_projection_kind)
                .unwrap_or(SlashActionKind::Command);
            let level = meta
                .and_then(|m| m.get("periLevel"))
                .and_then(|v| v.as_u64())
                .map(|l| l as u8)
                .filter(|l| *l == 1 || *l == 2)
                .unwrap_or(1);
            let args = meta
                .and_then(|m| m.get("periArgs"))
                .and_then(|v| serde_json::from_value::<ArgsSchema>(v.clone()).ok());
            let aliases = meta
                .and_then(|m| m.get("periAliases"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let category = meta
                .and_then(|m| m.get("periCategory"))
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(SlashCommandEntry {
                fullname,
                description,
                kind,
                level,
                args,
                aliases,
                category,
            })
        })
        .collect();
    Some(entries)
}

pub(super) fn decode_stream_update(params: &Value, session_id: &str) -> StreamUpdate {
    let Some(update) = params.get("update") else {
        return StreamUpdate::default();
    };
    let tag = update.get("sessionUpdate").and_then(Value::as_str);
    // ACP schema 的 ContentChunk / ToolCall / ToolCallUpdate 都标注了
    // #[serde(rename = "_meta")]，运行时 key 是 "_meta"（带下划线），不是 "meta"。
    // 检查顺序：_meta（生产格式）→ meta（兼容旧格式 / 测试格式）→ content._meta → content.meta
    let is_session_replay = update
        .get("_meta")
        .or_else(|| update.get("meta"))
        .or_else(|| update.get("content").and_then(|c| c.get("_meta")))
        .or_else(|| update.get("content").and_then(|c| c.get("meta")))
        .and_then(|m| m.get("periReplay"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // SubAgent identity uses ACP's typed params._meta extension field. Keep the
    // legacy params._peri lookup while older Peri servers may still emit it.
    let agent_id: Option<String> = params
        .get("_meta")
        .and_then(|meta| meta.get("peri"))
        .and_then(|peri| peri.get("sourceAgentId"))
        .or_else(|| {
            params
                .get("_peri")
                .and_then(|peri| peri.get("sourceAgentId"))
        })
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);

    tracing::debug!(
        target: "tui.acp_notifier",
        session_id = %session_id,
        agent_id = ?agent_id,
        "notifier: extracted agent_id from ACP metadata"
    );

    let mut token_count = None;
    let event = match tag {
        Some("agent_message_chunk") => {
            // ACP SDK ContentChunk wraps text in content.text, not at update top-level.
            // messageId is a top-level field on ContentChunk (alongside content) —
            // it carries the unique message identifier for each ReAct iteration.
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message_id = update
                .get("messageId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if is_session_replay {
                Some(AcpEventData::CommittedAssistantText {
                    text,
                    reasoning: None,
                })
            } else {
                let text_chunk = crate::kit::stream_data::TuiTextChunk {
                    text,
                    message_id,
                    agent_id,
                };
                Some(AcpEventData::TextChunk(text_chunk))
            }
        }
        Some("agent_thought_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message_id = update
                .get("messageId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if is_session_replay {
                Some(AcpEventData::CommittedAssistantText {
                    text: String::new(),
                    reasoning: Some(text),
                })
            } else {
                let reasoning_chunk = crate::kit::stream_data::TuiReasoningChunk {
                    text,
                    message_id,
                    agent_id,
                };
                Some(AcpEventData::ReasoningChunk(reasoning_chunk))
            }
        }
        Some("tool_call") => {
            let tool_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // ACP SDK ToolCall uses "title" field, not "name"
            let tool_name = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_summary = {
                let raw_input = update.get("rawInput").unwrap_or(&Value::Null);
                summarize_input(&tool_name, raw_input)
            };
            let raw_input = update.get("rawInput").cloned().unwrap_or(Value::Null);
            if is_session_replay {
                Some(AcpEventData::ReplayToolStarted {
                    tool_id,
                    tool_name,
                    input_summary,
                    raw_input,
                })
            } else {
                let tool_started = crate::kit::stream_data::TuiToolStarted {
                    tool_id,
                    tool_name,
                    input_summary,
                    raw_input,
                    agent_id,
                };
                Some(AcpEventData::ToolStarted(tool_started))
            }
        }
        Some("tool_call_update") => {
            let tool_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // ACP SDK ToolCallUpdate 使用 #[serde(flatten)] 将 rawOutput/status 合并到顶层；
            // 先尝试顶层字段（flatten 后的正确格式），再 fallback 到 fields 嵌套（兼容旧格式）。
            let output_summary = update
                .get("rawOutput")
                .or_else(|| update.get("fields").and_then(|f| f.get("rawOutput")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status = update
                .get("status")
                .or_else(|| update.get("fields").and_then(|f| f.get("status")))
                .and_then(|v| v.as_str());
            let Some(status) = status.filter(|status| matches!(*status, "completed" | "failed"))
            else {
                return StreamUpdate::default();
            };
            let is_error = status == "failed";
            if is_session_replay {
                Some(AcpEventData::ReplayToolEnded {
                    tool_id,
                    output_summary,
                    is_error,
                })
            } else {
                let tool_ended = crate::kit::stream_data::TuiToolEnded {
                    tool_id,
                    output_summary,
                    is_error,
                    agent_id,
                };
                Some(AcpEventData::ToolEnded(tool_ended))
            }
        }
        Some("usage_update") if !is_session_replay => {
            // UsageUpdate.meta 序列化 key 是 "_meta"（ACP SDK #[serde(rename = "_meta")]），
            // 带 fallback 兼容旧格式。
            let meta_obj = update.get("_meta").or_else(|| update.get("meta"));
            // Auxiliary usage is protocol-visible for progress/telemetry, but it
            // must never replace the parent turn's root cache sample.
            if agent_id.is_some() {
                return StreamUpdate::default();
            }
            let input = meta_obj
                .and_then(|m| m.get("inputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output = meta_obj
                .and_then(|m| m.get("outputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            token_count = Some((input + output) as usize);
            let cache_read = meta_obj
                .and_then(|m| m.get("cacheReadTokens"))
                .and_then(|v| v.as_u64());
            let request_id = meta_obj
                .and_then(|m| m.get("requestId"))
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned);
            // Missing cacheReadTokens: update spinner only; do not clear a prior
            // root sample (final usage_update often omits optional cache fields).
            match cache_read {
                None => None,
                Some(cached) if input == 0 || cached > input => {
                    Some(AcpEventData::CacheUsageUpdated(None))
                }
                Some(cached_tokens) => Some(AcpEventData::CacheUsageUpdated(Some(
                    crate::kit::acp_types::CacheUsageSample {
                        input_tokens: input,
                        cached_tokens,
                        request_id,
                    },
                ))),
            }
        }
        // ── session/replay: user_message_chunk ──
        // Session replay 通过 session/update 推送 user_message_chunk + agent_message_chunk，
        // 逐条重放历史。user_message_chunk 复用 LocalUserBubble（与手动输入走相同路径）。
        Some("user_message_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(input_id) = update.get("messageId").and_then(serde_json::Value::as_str) {
                Some(AcpEventData::ReplayedUserBubble {
                    input_id: input_id.to_owned(),
                    text,
                })
            } else {
                Some(AcpEventData::LocalUserBubble { text })
            }
        }
        _ => None, // unknown tags, including session_info_update (metadata-only, no stream event)
    };
    StreamUpdate { event, token_count }
}
