use crate::kit::stream_data::*;
use peri_acp_types::event_data::*;
use serde_json::Value;

// ---------------------------------------------------------------------------
// AcpEventData -- decoded ACP custom event
// ---------------------------------------------------------------------------

/// AcpEventData + active_session_id 包装类型。
/// active_session_id 从 ACP 通知的 session_id 字段提取。
/// acp_bridge 消费时与 state.active_session_id 比较以丢弃陈旧滞留事件。
#[derive(Debug, Clone)]
pub struct AcpEventWithEpoch {
    pub event: AcpEventData,
    pub active_session_id: String,
}

/// Decoded ACP custom event.
///
/// One variant per event name defined in the ACP protocol section 4
/// ("Event Directory", see `docs/design/peri-acp-protocol.md`).
///
/// The [`decode`](AcpEventData::decode) method maps a raw `{event, data}`
/// payload to the corresponding typed variant. Unknown event names are
/// captured as [`AcpEventData::Unknown`] for forward compatibility.
#[derive(Debug, Clone)]
pub enum AcpEventData {
    // -- §4.1 Streaming (high-frequency) ------------------------------------
    /// `"text-chunk"` -- incremental text for the current assistant bubble.
    TextChunk(TuiTextChunk),

    /// `"reasoning-chunk"` -- incremental reasoning / thinking text.
    ReasoningChunk(TuiReasoningChunk),

    /// `"tool-started"` -- creates an in-progress tool card.
    ToolStarted(TuiToolStarted),

    /// `"tool-ended"` -- fills in the tool card result.
    ToolEnded(TuiToolEnded),

    // -- §4.2 Boundary (low-frequency) -------------------------------------
    /// 本地提交已进入当前 ACP session，开始一轮真实 agent turn。
    PromptStarted,

    /// TUI 内部事件：用户已提交 prompt，loading spinner 应立即显示。
    /// submit_consumer 发出，bridge 收到后设 phase=PromptRunning, variant=1。
    /// `request_id` 为本轮 prompt RPC 的 id（submit_consumer 生成）——bridge
    /// 记录为"当前 turn 的 id"，供 stale TurnInterrupted 配对判定。
    PromptSubmitted { request_id: Option<String> },

    /// session/load 历史恢复开始。Replay 不是 agent turn，不能触发 loading。
    SessionReplayStarted,

    /// session/load 历史恢复结束。
    SessionReplayDone,

    /// `"turn-done"` -- agent finished this turn (Streaming -> Idle).
    TurnDone,

    /// `"turn-interrupted"` -- agent was interrupted (user cancel / timeout).
    /// `request_id` 为被中断 turn 的 prompt requestId（服务器经
    /// `peri/agent_event_done` 回带）——TUI 用它识别事件所属 turn，
    /// 丢弃早于当前 turn 的 stale 事件（Issue 2026-08-05）。
    TurnInterrupted {
        reason: String,
        request_id: Option<String>,
    },

    /// `"turn-suspended"` -- agent turn suspended, waiting for bg agent/cron/workflow.
    /// TUI 收到后应归档 current_turn + 停止 loading spinner。
    /// Agent 保持存活（await_wake），新 turn 的流事件自动恢复 loading。
    TurnSuspended,

    /// TUI 内部事件：本地用户提交的 UserBubble。仅 TUI 内部使用，不走 ACP 协议。
    LocalUserBubble { text: String },

    /// TUI 内部事件：本地 loading 复位请求（cancel / /clear / prompt 失败
    /// 兜底时由 submit_consumer 发出）。仅 TUI 内部使用，不走 ACP 协议。
    /// bridge 收到后若 phase == PromptRunning 则复位为 Idle 并重推 ACP_STATE
    /// ——与直接写 ACP_STATE.is_loading 的兜底互补：兜底覆盖 bridge 已退出的
    /// shutdown 路径，本事件覆盖 bridge 存活时 phase 派生覆盖（cancel 后迟到
    /// 事件触发 push_acp_state 会用 phase 重算 is_loading=true，造成闪回）。
    /// 幂等：phase 非 PromptRunning 时 no-op（Issue 2026-08-05 S4.2）。
    LocalLoadingReset,

    /// bg agent 完成回调 user bubble——要求先 flush current_turn 到 committed，
    /// 再 push 自身。与 LocalUserBubble 的纯追加不同，此变体主动切分视觉 turn：
    /// 在 agent ReAct 循环中间插入用户气泡，把同一轮 TurnDone 的 AI 内容
    /// 分割为「bg 回调前」和「bg 回调后」两段。
    BgCallbackBubble { text: String },

    /// TUI 内部事件：直接将完整 AI 文本气泡追加到 committed。
    /// 用于 session/load replay 及任何需要旁路 current_turn 直接归档的场景。
    /// `reasoning` 非空时会创建独立的 reasoning 折叠块。
    CommittedAssistantText {
        text: String,
        reasoning: Option<String>,
    },

    /// replay 工具调用开始——直接写入 committed 的 TuiToolCard（is_running=true）。
    ReplayToolStarted {
        tool_id: String,
        tool_name: String,
        input_summary: String,
        raw_input: Value,
    },

    /// replay 工具调用结束——更新 committed 中对应 tool_id 的 TuiToolCard。
    ReplayToolEnded {
        tool_id: String,
        output_summary: String,
        is_error: bool,
    },

    // -- §4.3 Status (status bar updates) ----------------------------------
    /// `"tool-count"` -- number of tool calls in the current turn.
    ToolCount(ToolCount),

    /// `"progress"` -- progress percentage with label.
    Progress(Progress),

    /// `"budget-warning"` -- context budget threshold crossed.
    BudgetWarning(BudgetWarning),

    /// `"system-notification"` -- system-level notification text with severity.
    SystemNotification(SystemNotification),

    /// `"command-feedback"` — 命令执行反馈（Phase 1 `CommandFeedback` 载荷，
    /// 经 peri/agent_event 通道送达，无标准 SessionUpdate；level/channel 为
    /// wire string 化 camelCase：`"info"|"warning"|"error"`、`"uiOnly"|"session"`）。
    CommandFeedback(TuiCommandFeedback),

    // -- §4.4 Input assist -------------------------------------------------
    /// `"prediction"` -- input prediction suggestion (grey placeholder).
    Prediction(Prediction),

    /// `"file-suggestions"` -- @-mention file completion candidates.
    FileSuggestions(FileSuggestions),

    // -- §4.5 Interaction requests (require user decision) ------------------
    /// `"hitl-pending"` -- HITL tool approval request.
    HitlPending(HitlPending),

    /// `"ask-user"` -- multi-question form initiated by the agent.
    AskUser(AskUser),

    /// TUI 内部事件（Slice 4 §6.8）：interaction block 结果回写。仅 TUI 内部
    /// 使用，不走 ACP 协议——`ask_user_action` / `hitl_response` 消费者在
    /// 提交/拒绝后发出，bridge 扫描 committed 中 pending 的 `TuiAskUserBlock`
    /// 按 `request_id` 匹配，clone + `pending=false` + `result` + 重算 hash +
    /// 原位 set（COW）。`result` 为渲染文案（纯文本，无符号——渲染层负责
    /// 加状态符号与颜色）。
    InteractionResolved { request_id: String, result: String },

    /// `"rewind-preview"` -- preview of changes that will be undone.
    RewindPreview(RewindPreview),

    /// Rewind 已完成——messages_json 为 BaseMessage 数组的 JSON。
    /// 由 AcpEvent::RewindCompleted（peri/agent_event）转换而来，
    /// dispatch_and_notify 反序列化后替换 state.committed。
    RewindCompleted { messages_json: String },

    /// `"oauth-needed"` -- MCP server authorization required.
    OauthNeeded(OauthNeeded),

    /// `"oauth-completed"` -- MCP OAuth 授权完成（授权码流程走完/回调成功）。
    /// 由 AcpEvent::OauthCompleted（peri/agent_event）转换而来。
    OauthCompleted { server_name: String },

    /// `"oauth-failed"` -- MCP OAuth 授权失败（超时/取消/服务端拒绝）。
    /// 由 AcpEvent::OauthFailed（peri/agent_event）转换而来。
    OauthFailed { server_name: String, error: String },

    /// `"oauth-restored"` -- MCP OAuth 凭证恢复成功（快速路径：磁盘已有
    /// 有效凭证，跳过浏览器授权）。由 AcpEvent::OauthRestored 转换而来。
    OauthRestored { server_name: String },

    // -- §4.6 Structure (control message-area layout) ------------------------
    /// `"subagent-started"` -- sub-agent created, TUI opens a collapsible group.
    SubagentStarted {
        agent_id: String,
        agent_name: String,
        is_background: bool,
    },

    /// `"subagent-stopped"` -- sub-agent exited, TUI closes the group.
    /// `result` / `is_error` 为 parent 终态的唯一事实源（canonical）：
    /// nested child tool error 不参与 parent 状态判定。
    SubagentStopped {
        agent_id: String,
        result: String,
        is_error: bool,
    },

    /// Fallback for unknown / future event names.
    ///
    /// Keeps the raw event name and JSON data so the state machine can log or
    /// silently ignore new events without crashing.
    Unknown {
        event: String,
        data: serde_json::Value,
    },

    // -- §4.7 Background Tasks (bg-task-*) ----------------------------------
    /// `"bg-task-started"` -- a background task has been registered.
    BgTaskStarted(BgTaskEntry),

    /// `"bg-task-completed"` -- a background task has finished.
    BgTaskCompleted {
        task_id: String,
        kind: Option<String>,
        success: bool,
        duration_ms: u64,
    },

    /// `"bg-task-cancelled"` -- a background task was cancelled.
    BgTaskCancelled { task_id: String, reason: String },

    /// `"bg-task-snapshot"` -- full list of active background tasks.
    BgTaskSnapshot(Vec<BgTaskEntry>),

    // -- §4.8 Agent Event Extensions (P1-5) ----------------------------------
    /// `"turn-committed"` — ReAct 迭代提交信号。
    TurnCommitted { messages_json: String, steps: usize },

    /// `"compact-started"` — 上下文压缩开始。
    CompactStarted,

    /// `"compact-completed"` — 上下文压缩完成（Phase 5 Step 4 收敛：
    /// 状态重建信号三字段；通知文案由 CommandFeedback 渲染）。
    CompactCompleted {
        summary: String,
        messages_json: String,
        /// 压缩触发方式: "auto" | "manual"（旧事件缺省视为 "auto"，由
        /// acp_notifier 透传时补默认值）
        trigger: String,
    },

    /// `"background-task-completed"` — 后台 agent 任务完成。
    BackgroundTaskCompleted {
        task_id: String,
        agent_name: String,
        success: bool,
        output: String,
        tool_calls_count: usize,
        duration_ms: u64,
        child_thread_id: Option<String>,
    },

    /// `"agent-execution-failed"` — agent 执行失败。
    AgentExecutionFailed { message: String },

    /// `"workflow-progress"` — 工作流进度更新。
    WorkflowProgress {
        run_id: String,
        workflow_name: String,
        event_type: String,
        agent_id: Option<u64>,
        phase: Option<String>,
        label: Option<String>,
        agent_status: Option<String>,
        token_count: Option<u64>,
        tool_count: Option<u64>,
        run_status: Option<String>,
        message: Option<String>,
    },

    // -- §4.9 Plugin events ------------------------------------------------
    /// `"plugin-snapshot"` — 插件列表全量快照。
    PluginSnapshot(PluginSnapshot),
    /// `"plugin-action-result"` — 插件操作结果通知。
    PluginActionResult(PluginActionResult),
    /// `"plugin-search-result"` — Discover 搜索返回。
    PluginSearchResult(PluginSearchResult),
}

impl AcpEventData {
    /// Decode a raw `{event, data}` payload into a typed [`AcpEventData`].
    ///
    /// Dispatches by event name (kebab-case string). On deserialization
    /// failure or unknown event name, falls back to [`AcpEventData::Unknown`].
    pub fn decode(event: &str, data: serde_json::Value) -> Self {
        match event {
            // §4.1 Streaming -- deprecated, now delivered via session/update
            // "text-chunk", "reasoning-chunk", "tool-started", "tool-ended" 解码分支已移除。
            // 流式事件现在由 handle_session_update（acp_notifier.rs）处理。

            // §4.2 Boundary
            "turn-done" => AcpEventData::TurnDone,
            "turn-interrupted" => {
                let reason = data["reason"].as_str().unwrap_or("").to_string();
                // requestId 为可选字段：unstable_event 通道当前未发射 turn-interrupted
                // （router 返回 None），保留解析以兼容未来启用（协议文档已标注）。
                let request_id = data["requestId"].as_str().map(String::from);
                AcpEventData::TurnInterrupted { reason, request_id }
            }
            "turn-suspended" => AcpEventData::TurnSuspended,

            // §4.3 Status
            "tool-count" => decode_or_unknown(event, data, AcpEventData::ToolCount),
            "progress" => decode_or_unknown(event, data, AcpEventData::Progress),
            "budget-warning" => decode_or_unknown(event, data, AcpEventData::BudgetWarning),
            "system-notification" => {
                decode_or_unknown(event, data, AcpEventData::SystemNotification)
            }

            // §4.4 Input assist
            "prediction" => decode_or_unknown(event, data, AcpEventData::Prediction),
            "file-suggestions" => decode_or_unknown(event, data, AcpEventData::FileSuggestions),

            // §4.5 Interaction requests
            "rewind-preview" => decode_or_unknown(event, data, AcpEventData::RewindPreview),
            "rewind-completed" => {
                let messages_json = data["messages_json"].as_str().unwrap_or("").to_string();
                AcpEventData::RewindCompleted { messages_json }
            }
            "oauth-needed" => decode_or_unknown(event, data, AcpEventData::OauthNeeded),
            "oauth-completed" => {
                let server_name = data["server_name"].as_str().unwrap_or("").to_string();
                AcpEventData::OauthCompleted { server_name }
            }
            "oauth-failed" => {
                let server_name = data["server_name"].as_str().unwrap_or("").to_string();
                let error = data["error"].as_str().unwrap_or("").to_string();
                AcpEventData::OauthFailed { server_name, error }
            }
            "oauth-restored" => {
                let server_name = data["server_name"].as_str().unwrap_or("").to_string();
                AcpEventData::OauthRestored { server_name }
            }

            // §4.6 Structure
            "subagent-started" => {
                let agent_id = data["agent_id"].as_str().unwrap_or("").to_string();
                let agent_name = data["agent_name"].as_str().unwrap_or("").to_string();
                let is_background = data["is_background"].as_bool().unwrap_or(false);
                AcpEventData::SubagentStarted {
                    agent_id,
                    agent_name,
                    is_background,
                }
            }
            "subagent-stopped" => {
                let agent_id = data["agent_id"].as_str().unwrap_or("").to_string();
                // result/is_error 为可选字段——legacy 通道缺省空字符串/false，
                // 保持向后兼容（canonical 主通道为 peri/agent_event）。
                let result = data["result"].as_str().unwrap_or("").to_string();
                let is_error = data["is_error"].as_bool().unwrap_or(false);
                AcpEventData::SubagentStopped {
                    agent_id,
                    result,
                    is_error,
                }
            }

            // §4.7 Background Tasks
            "bg-task-started" => decode_or_unknown(event, data, AcpEventData::BgTaskStarted),
            "bg-task-completed" => decode_or_unknown(event, data, |d: BgTaskCompletedData| {
                AcpEventData::BgTaskCompleted {
                    task_id: d.task_id,
                    kind: d.kind,
                    success: d.success,
                    duration_ms: d.duration_ms,
                }
            }),
            "bg-task-cancelled" => decode_or_unknown(event, data, |d: BgTaskCancelledData| {
                AcpEventData::BgTaskCancelled {
                    task_id: d.task_id,
                    reason: d.reason,
                }
            }),
            "bg-task-snapshot" => decode_or_unknown(event, data, AcpEventData::BgTaskSnapshot),

            "bg-callback-user-message" => {
                let text = data["text"].as_str().unwrap_or("").to_string();
                AcpEventData::BgCallbackBubble { text }
            }

            // -- §4.9 Plugin events ----------------------------------------
            "plugin-snapshot" => decode_or_unknown(event, data, AcpEventData::PluginSnapshot),
            "plugin-action-result" => {
                decode_or_unknown(event, data, AcpEventData::PluginActionResult)
            }
            "plugin-search-result" => {
                decode_or_unknown(event, data, AcpEventData::PluginSearchResult)
            }

            // Unknown / future event names -- forward-compatible fallback.
            _ => AcpEventData::unknown(event, data),
        }
    }

    /// Helper to construct the [`AcpEventData::Unknown`] variant.
    fn unknown(event: &str, data: serde_json::Value) -> Self {
        AcpEventData::Unknown {
            event: event.to_owned(),
            data,
        }
    }
}

// ---------------------------------------------------------------------------
// TuiCommandFeedback -- decoded CommandFeedback event payload
// ---------------------------------------------------------------------------

/// 命令执行反馈（Phase 1 `CommandFeedback` 的 TUI 侧结构化镜像）。
/// 经 `convert_agent_event`（peri/agent_event 通道）解析 wire 字符串后构造。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiCommandFeedback {
    pub level: FeedbackLevel,
    pub message: String,
    pub channel: FeedbackChannel,
}

/// 反馈级别（wire: `"info"|"warning"|"error"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackLevel {
    Info,
    Warning,
    Error,
}

/// 反馈通道（wire: `"uiOnly"|"session"`；缺省 UiOnly）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackChannel {
    UiOnly,
    Session,
}

// ---------------------------------------------------------------------------
// BgTaskEntry -- TUI-side background task entry
// ---------------------------------------------------------------------------

/// Background task entry mirroring `BgTaskInfo` from the agent layer.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BgTaskEntry {
    pub task_id: String,
    pub kind: String,
    pub summary: String,
    pub started_at: String,
    pub pid: Option<u32>,
}

/// Deserialization helper for `bg-task-completed` payload.
#[derive(Debug, serde::Deserialize)]
struct BgTaskCompletedData {
    task_id: String,
    #[serde(default)]
    kind: Option<String>,
    success: bool,
    duration_ms: u64,
}

/// Deserialization helper for `bg-task-cancelled` payload.
#[derive(Debug, serde::Deserialize)]
struct BgTaskCancelledData {
    task_id: String,
    reason: String,
}

/// Decode `data` into `T` and apply the variant constructor, or fall back to
/// [`AcpEventData::Unknown`] with the original `data` preserved.
fn decode_or_unknown<T, F>(event: &str, data: serde_json::Value, ctor: F) -> AcpEventData
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> AcpEventData,
{
    match serde_json::from_value::<T>(data.clone()) {
        Ok(v) => ctor(v),
        Err(_) => AcpEventData::unknown(event, data),
    }
}
