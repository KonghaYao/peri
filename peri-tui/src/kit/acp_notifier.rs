//! ACP notifier——AcpNotification → AcpEventData 转换器。
//!
//! 直接在 notifier 内完成 DTO 转换，产出的 `AcpEventData` 立即送入 `spawn_acp_bridge`。
//! - **以 session/update 为流式主通道**：ACP 服务端的高频流式事件
//!   （agent_message_chunk / agent_thought_chunk / tool_call / tool_call_update）
//!   通过标准 `session/update` 携带，在 `handle_session_update` 中转换为
//!   `AcpEventData` 变体推入双 bridge channel。
//! - **usage_update**：token 消耗通过标准 session/update 的 `usage_update` tag
//!   携带，直接写入 `SPINNER_TOKEN_COUNT` atom，不产生 AcpEventData。
//! - **AgentEvent DTO 已接入**：`peri/agent_event` 携带的 AcpEvent 变体
//!   （SubagentStarted/SubagentStopped/TurnSuspended/RewindCompleted/...）
//!   通过 `convert_agent_event` 转换为 AcpEventData 推入双 bridge channel。
//!   未映射变体（StateSnapshot/BgToolStep/LspDiagnostics/ContextWarning/...）
//!   保持静默丢弃，S5+ 迭代扩展。
//!
//! 该任务是**纯转换 + channel push**——不做状态突变。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::acp_client::AcpNotification;
use crate::i18n;
use crate::kit::acp_types::{
    AcpEventData, AcpEventWithEpoch, FeedbackChannel, FeedbackLevel, TuiCommandFeedback,
};
use crate::kit::atoms::{
    ACP_STATE, ASK_USER_REQUEST_ID, AVAILABLE_SLASH_COMMANDS, HITL_REQUEST_ID, INPUT_BUFFER,
    NOTIFICATION, PERI_CONFIG_HANDLE, RENDER_HEARTBEAT, SPINNER_TOKEN_COUNT,
};
use crate::kit::input_area::refresh_slash_items;
use crate::kit::slash_completion::SlashActionKind;
use crate::kit::slash_projection::{ArgsSchema, SlashCommandEntry, parse_projection_kind};
use crate::truncate::summarize_input;
use fluent_bundle::FluentValue;
use peri_acp::event::AcpEvent;
use peri_acp_types::event_data::{
    AskUser, HitlPending, OauthNeeded, Question, QuestionOption, SystemNotification,
};
use serde_json::Value;

/// 启动 kit ACP notifier 后台任务。
///
/// 从 `notification_rx` 读取 `AcpNotification`，把可识别的流式事件转换为
/// `AcpEventData` 推入 `bridge_tx`，由 `spawn_acp_bridge` 消费并写入 Atom。
///
/// 通道关闭（transport 断开）或 shutdown 触发时干净退出。
pub fn spawn_kit_notifier(
    mut notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!("kit ACP notifier: shutdown signal received, exiting");
                    break;
                }
                n = notification_rx.recv() => {
                    match n {
                        Some(notif) => forward_notification(&bridge_tx, notif),
                        None => {
                            debug!("kit ACP notifier: notification channel closed (transport disconnected)");
                            // Issue 2026-08-05: 事件流中断兜底复位。
                            // transport 死亡后不再有任何事件到达，is_loading 的所有
                            // 复位路径都依赖事件，会永久卡 true 锁死 TUI（Ctrl+C 退出、
                            // /exit、/clear 全被 loading 门禁拦截）。此处直接复位 atom：
                            // 清 loading + 排队输入 + 提示断连（app-agent-disconnected
                            // 文案此前在 FTL 中存在但零引用，此处接上）。
                            ACP_STATE.state().write().is_loading = false;
                            INPUT_BUFFER.state().write().clear();
                            *NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
                                message: i18n::tr("app-agent-disconnected"),
                                until: std::time::Instant::now() + std::time::Duration::from_secs(5),
                            });
                            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                            break;
                        }
                    }
                }
            }
        }
    })
}

/// 将 `peri/agent_event` 通道的 `AcpEvent` DTO 转换为 kit 层的 `AcpEventData`。
///
/// 当前映射列表（需与后续 S5+ 迭代同步扩展）：
/// - `SubagentStarted` / `SubagentStopped` / `LlmRetrying` → 对应的 `AcpEventData` 变体
/// - 其他变体返回 `None`（不存在对应的 `AcpEventData` 或以其他通道覆盖）
fn convert_agent_event(event: AcpEvent) -> Option<AcpEventData> {
    match event {
        AcpEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => Some(AcpEventData::SubagentStarted {
            agent_id: instance_id,
            agent_name,
            is_background,
        }),
        AcpEvent::SubagentStopped {
            instance_id,
            result,
            is_error,
            ..
        } => Some(AcpEventData::SubagentStopped {
            agent_id: instance_id,
            result,
            is_error,
        }),
        // ── §4.8 Agent Event Extensions (P1-5) ──
        AcpEvent::TurnCommitted {
            messages_json,
            steps,
        } => Some(AcpEventData::TurnCommitted {
            messages_json,
            steps,
        }),
        AcpEvent::CompactStarted => Some(AcpEventData::CompactStarted),
        AcpEvent::CompactCompleted {
            summary,
            messages_json,
            trigger,
        } => Some(AcpEventData::CompactCompleted {
            summary,
            messages_json,
            trigger,
        }),
        AcpEvent::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        } => Some(AcpEventData::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        }),
        AcpEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => Some(AcpEventData::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        }),
        AcpEvent::AgentExecutionFailed { message } => {
            Some(AcpEventData::AgentExecutionFailed { message })
        }
        AcpEvent::WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            agent_id,
            phase,
            label,
            agent_status,
            token_count,
            tool_count,
            run_status,
            message,
        } => Some(AcpEventData::WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            agent_id,
            phase,
            label,
            agent_status,
            token_count,
            tool_count,
            run_status,
            message,
        }),
        AcpEvent::RewindCompleted {
            messages_json,
            summary: _,
        } => Some(AcpEventData::RewindCompleted { messages_json }),
        // SystemNotification：MCP 上下线等连接状态变化（peri/agent_event 通道
        // 送达），转换为 AcpEventData::SystemNotification 显示系统通知。
        AcpEvent::SystemNotification { text, level } => {
            Some(AcpEventData::SystemNotification(SystemNotification {
                text,
                level,
            }))
        }
        // CommandFeedback：命令执行反馈（Phase 3 事件链路，经 peri/agent_event
        // 通道送达，无标准 SessionUpdate）。level/channel 为 wire string 化
        // camelCase（"info" / "uiOnly"），解析为结构化枚举后推入 dual-bridge。
        // 未知 level 回落 Info、未知 channel 回落 UiOnly（Phase 1 缺省语义）。
        AcpEvent::CommandFeedback {
            level,
            message,
            channel,
        } => Some(AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: match level.as_str() {
                "warning" => FeedbackLevel::Warning,
                "error" => FeedbackLevel::Error,
                _ => FeedbackLevel::Info,
            },
            message,
            channel: match channel.as_str() {
                "session" => FeedbackChannel::Session,
                _ => FeedbackChannel::UiOnly,
            },
        })),
        // TurnSuspended：bg agent/cron/workflow 挂起信号——归档 current_turn、
        // 停止 loading spinner。双轨下线后（2026-08-05-3.0-m-event-chain-canonical）
        // 此信号仅经 ACP peri/agent_event 通道送达。
        AcpEvent::TurnSuspended { .. } => Some(AcpEventData::TurnSuspended),
        // OAuth 授权事件（host 级，跨 session）：OauthNeeded 打开 popup 收集
        // 授权码，Completed/Failed 关闭 popup 并提示结果。
        AcpEvent::OauthNeeded {
            server_name,
            auth_url,
        } => Some(AcpEventData::OauthNeeded(OauthNeeded {
            server_name,
            auth_url,
        })),
        AcpEvent::OauthCompleted { server_name } => {
            Some(AcpEventData::OauthCompleted { server_name })
        }
        AcpEvent::OauthFailed { server_name, error } => {
            Some(AcpEventData::OauthFailed { server_name, error })
        }
        AcpEvent::OauthRestored { server_name } => {
            Some(AcpEventData::OauthRestored { server_name })
        }
        // StateSnapshotMeta：从 budget_pct 写入 CONTEXT_USAGE atom（供 StatusBarRow1 显示）
        AcpEvent::StateSnapshotMeta {
            context_total_tokens,
            budget_pct,
            ..
        } => {
            if let Some(total) = context_total_tokens {
                // budget_pct 可能为 None（首轮/token_tracker 无 last_usage），此时仅存总量
                let pct = budget_pct.unwrap_or(0.0);
                *crate::kit::atoms::CONTEXT_USAGE.state().write() = Some((pct, total));
                crate::kit::atoms::RENDER_HEARTBEAT
                    .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
            }
            None
        }
        _ => {
            debug!("kit ACP notifier: AcpEvent variant not yet mapped to AcpEventData, dropping");
            None
        }
    }
}

/// 把单条 `AcpNotification` 转换并推入 bridge channel。
///
/// 设计决策：session/update 是流式主通道（agent_message_chunk / tool_call 等），
/// AgentDone 通过 TurnDone 转换，AgentEvent 通过 `convert_agent_event` 转换。
fn forward_notification(bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>, n: AcpNotification) {
    /// 将 AcpEventData 包装为 AcpEventWithEpoch（注入 session_id）。
    fn wrap_with_session(event: AcpEventData, session_id: String) -> AcpEventWithEpoch {
        AcpEventWithEpoch {
            event,
            active_session_id: session_id,
        }
    }

    match n {
        AcpNotification::UnstableEvent {
            session_id,
            event,
            data,
        } => {
            let decoded = AcpEventData::decode(&event, data);
            if matches!(decoded, AcpEventData::Unknown { .. }) {
                debug!(event = %event, "kit ACP notifier: unknown unstable_event, dropping");
                return;
            }
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping event");
            }
        }
        // kit notifier: extract AvailableCommandsUpdate / plan / streaming
        // from SessionUpdate.
        AcpNotification::SessionUpdate { session_id, params } => {
            if let Some(decoded) = handle_session_update(params, bridge_tx, &session_id) {
                let wrapped = wrap_with_session(decoded, session_id);
                if let Err(e) = bridge_tx.send(wrapped) {
                    warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping session/update streaming event");
                }
            }
        }
        AcpNotification::AgentDone {
            session_id,
            stop_reason,
            request_id,
        } => {
            let decoded = if stop_reason == "cancelled" {
                AcpEventData::TurnInterrupted {
                    reason: "user cancelled".into(),
                    // 透传被中断 turn 的 requestId——bridge 据此识别事件所属 turn，
                    // 丢弃早于当前 turn 的 stale 取消事件（Issue 2026-08-05）。
                    request_id,
                }
            } else {
                AcpEventData::TurnDone
            };
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping agent done");
            }
        }
        AcpNotification::Elicitation { id, params } => {
            handle_elicitation(&id, &params, bridge_tx);
        }
        // peri/agent_event → AcpEvent → AcpEventData 转换
        // SubagentStarted/SubagentStopped 首先映射至此通道；通过 convert_agent_event
        // 转换为 kit 层 DTO 后推送（与 UnstableEvent 路径形成双通道冗余）。
        AcpNotification::AgentEvent { session_id, event } => {
            if let Some(decoded) = convert_agent_event(event) {
                let wrapped = wrap_with_session(decoded, session_id);
                if let Err(e) = bridge_tx.send(wrapped) {
                    warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping AgentEvent");
                }
            }
        }
        AcpNotification::PredictionReady {
            session_id,
            text,
            actions,
        } => {
            // M4: PredictionReady 不再被丢弃，转换为 AcpEventData::Prediction 推入 bridge channel。
            // dispatch_and_notify 仅写入 PREDICTION atom（input_area 订阅显示），不调
            // push_view_models。
            use peri_acp_types::event_data::Prediction;
            let decoded = AcpEventData::Prediction(Prediction { text, actions });
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping prediction");
            }
        }
        AcpNotification::RequestPermission { id, params } => {
            handle_request_permission(&id, &params, bridge_tx);
        }
        AcpNotification::Peri { .. } | AcpNotification::Other { .. } => {
            debug!("kit ACP notifier: notification variant not yet handled, dropping");
        }
    }
}

/// Extract commands / plan / streaming events from a SessionUpdate notification.
///
/// Returns `Some(AcpEventData)` for streaming tags (agent_message_chunk,
/// agent_thought_chunk, tool_call, tool_call_update) so the caller can push
/// to the dual-bridge channel. Returns `None` for status-only updates
/// (available_commands_update, plan, usage_update).
fn handle_session_update(
    params: serde_json::Value,
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    session_id: &str,
) -> Option<AcpEventData> {
    // params: {"session_id": "...", "update": <SessionUpdate>}
    // SessionUpdate uses #[serde(tag = "sessionUpdate", rename_all = "snake_case")]
    // → AvailableCommandsUpdate serializes as:
    //   {"sessionUpdate": "available_commands_update", "availableCommands": [...]}
    let update = match params.get("update") {
        Some(u) => u,
        None => return None,
    };
    // Discriminate: check the tag field, not a container key
    let tag = update.get("sessionUpdate").and_then(|v| v.as_str());

    if tag == Some("available_commands_update") {
        let cmds = match update.get("availableCommands").and_then(|v| v.as_array()) {
            Some(c) => c,
            None => return None,
        };
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
        let len = entries.len();
        *AVAILABLE_SLASH_COMMANDS.state().write() = entries;
        // 单 atom 原子写后刷新补全缓存（无旧分类残留）。
        refresh_slash_items();
        debug!(
            "kit ACP notifier: updated AVAILABLE_SLASH_COMMANDS ({})",
            len
        );
        return None;
    }

    if tag == Some("plan") {
        debug!(update = %update, "handle_session_update: plan tag matched");
        crate::kit::acp_events::handle_plan_update(update);
        return None;
    }

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

    match tag {
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
                return None;
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
            let input = meta_obj
                .and_then(|m| m.get("inputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output = meta_obj
                .and_then(|m| m.get("outputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            *SPINNER_TOKEN_COUNT.state().write() = (input + output) as usize;
            // 缓存命中率：低于 80% 时直接 push SystemNotification 到消息流。
            // 受 AppConfig.show_cache_warning 控制（config 面板开关，默认关闭）。
            let cache_read = meta_obj
                .and_then(|m| m.get("cacheReadTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if cache_read > 0 && input > 0 {
                let show_warning = PERI_CONFIG_HANDLE
                    .get()
                    .map(|h| h.read().config.show_cache_warning.unwrap_or(false))
                    .unwrap_or(false);
                if !show_warning {
                    return None;
                }
                let hit_rate = cache_read as f64 / input as f64;
                if hit_rate < 0.8 {
                    let req_id = meta_obj
                        .and_then(|m| m.get("requestId"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("-");
                    let pct = (hit_rate * 100.0) as u32;
                    let text = i18n::tr_args(
                        "app-note-cache-hit-low",
                        &[
                            ("pct".into(), FluentValue::from(pct as u64)),
                            ("req_id".into(), FluentValue::from(req_id)),
                        ],
                    );
                    let data = SystemNotification {
                        text,
                        level: "warning".into(),
                    };
                    let event = AcpEventData::SystemNotification(data);
                    let wrapped = AcpEventWithEpoch {
                        event,
                        active_session_id: session_id.to_string(),
                    };
                    if let Err(e) = bridge_tx.send(wrapped) {
                        warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping cache warning");
                    }
                }
            }
            None
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
            Some(AcpEventData::LocalUserBubble { text })
        }
        _ => None, // unknown tags, including session_info_update (metadata-only, no stream event)
    }
}

/// 处理 Elicitation 通知：解析 params 为 AskUser → 写入 ASK_USER_REQUEST_ID atom →
/// 构造 AcpEventData::AskUser 推入双 bridge。
fn handle_elicitation(
    id: &peri_acp::transport::types::RequestId,
    params: &Value,
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
) {
    // 从 params 中提取 session_id
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 序列化 RequestId 存入 atom（供 popup 提交时回传）
    if let Ok(id_str) = serde_json::to_string(id) {
        *ASK_USER_REQUEST_ID.state().write() = Some(id_str);
    } else {
        warn!("kit ACP notifier: failed to serialize elicitation RequestId");
        return;
    }

    let questions = parse_elicitation_questions(params);
    let ask_user = AskUser { questions };
    let event = AcpEventData::AskUser(ask_user);
    let wrapped = AcpEventWithEpoch {
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding Elicitation as AskUser event");

    if let Err(e) = bridge_tx.send(wrapped) {
        warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping AskUser");
    }
}

/// 处理 RequestPermission 通知（HITL）：解析 params 为 HitlPending →
/// 写入 HITL_REQUEST_ID atom（供 HitlPopup 回传）→ 构造 AcpEventData::HitlPending
/// 推入双 bridge channel，由 dispatch_and_notify 写入 HITL_PENDING atom + 设 POPUP_KIND。
///
/// JSON 结构（CreatePermissionRequest ACP schema）:
/// ```json
/// {"sessionId": "sess_1", "toolCall": {"title": "Bash", "rawInput": {...}},
///  "options": [{"id": "allow_once", ...}, ...]}
/// ```
fn handle_request_permission(
    id: &peri_acp::transport::types::RequestId,
    params: &Value,
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
) {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if let Ok(id_str) = serde_json::to_string(id) {
        *HITL_REQUEST_ID.state().write() = Some(id_str);
    } else {
        warn!("kit ACP notifier: failed to serialize RequestPermission RequestId");
        return;
    }

    // 从 params.toolCall 提取 tool_name + tool_input
    let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
    let tool_name = tool_call
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tool_input = tool_call.get("rawInput").cloned().unwrap_or(Value::Null);
    let hp = HitlPending {
        tool_name,
        tool_input,
        batch: None,
    };

    let event = AcpEventData::HitlPending(hp);
    let wrapped = AcpEventWithEpoch {
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding RequestPermission as HitlPending event");

    if let Err(e) = bridge_tx.send(wrapped) {
        warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping HitlPending");
    }
}

/// 从 CreateElicitationRequest JSON 中解析问题列表。
///
/// JSON 结构（CreateElicitationRequest 序列化后，#[serde(flatten)] 展开）:
/// ```json
/// {"mode": "form", "sessionId": "sess_1", "message": "...",
///  "requestedSchema": {"type": "object", "properties": {
///   "q_id": {"type": "string", "title": "Header", "description": "Question text",
///            "oneOf": [{"const": "label", "title": "label"}]},
///   "multi_q_id": {"type": "array", "title": "...", "description": "...",
///                  "items": {"anyOf": [{"const": "label", "title": "..."}]}}
/// }}}
/// ```
///
/// 解析失败时返回空 Vec（弹窗显示 "0 questions"）。
fn parse_elicitation_questions(params: &Value) -> Vec<Question> {
    let props = match params
        .get("requestedSchema")
        .and_then(|rs| rs.get("properties"))
        .and_then(|p| p.as_object())
    {
        Some(p) => p,
        None => {
            warn!("kit ACP notifier: elicitation params missing requestedSchema.properties");
            return vec![];
        }
    };

    props
        .iter()
        .map(|(id, prop)| {
            let header = prop
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let question = prop
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let prop_type = prop.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match prop_type {
                "array" => {
                    // multi_select: options 在 items.anyOf
                    let options = extract_options_from_oneof(prop, "anyOf", true);
                    Question {
                        id: id.clone(),
                        question,
                        header,
                        options,
                        multi_select: true,
                    }
                }
                "string" => {
                    // single select: options 在 oneOf
                    let options = extract_options_from_oneof(prop, "oneOf", false);
                    Question {
                        id: id.clone(),
                        question,
                        header,
                        options,
                        multi_select: false,
                    }
                }
                _ => Question {
                    id: id.clone(),
                    question,
                    header,
                    options: vec![],
                    multi_select: false,
                },
            }
        })
        .collect()
}

/// 从 prop["items"][key] 或 prop[key] 中提取 QuestionOption 列表。
/// - `nested=true`：选项在 `prop["items"][key]`（multi_select / anyOf）
/// - `nested=false`：选项在 `prop[key]`（single_select / oneOf）
fn extract_options_from_oneof(prop: &Value, key: &str, nested: bool) -> Vec<QuestionOption> {
    let arr = if nested {
        prop.get("items").and_then(|items| items.get(key))
    } else {
        prop.get(key)
    }
    .and_then(|v| v.as_array());

    let Some(arr) = arr else {
        return vec![];
    };

    arr.iter()
        .map(|opt| QuestionOption {
            label: opt
                .get("const")
                .or_else(|| opt.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            description: opt
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

#[cfg(test)]
#[path = "acp_notifier_test.rs"]
mod tests;
