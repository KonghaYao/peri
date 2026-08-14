//! ACPChannel：入站规范化（架构 §6.1，设计稿 `f5-channel-control.md` §3）。
//!
//! instance 透传的原始 ACP 帧（`{type,payload}` 或 JSON-RPC `session/update`
//! 包裹）在此规范化为 [`NormalizedEvent`]。**纯函数**：无 I/O、无日志副作用；
//! binding 校验与持久化在调用方（RelayEventHandler）。
//!
//! 双格式兼容（§6.1）：原始 `{ type, payload }` 与 JSON-RPC `session/update`
//! （含包裹格式）统一提取；JSON-RPC **response**（有 `id`、无 `method`）走
//! 专门面 [`NormalizeOutcome::RpcResponse`]（L3 确认，不产生业务事件）。
//!
//! 未知 type / 未知 method → [`NormalizeOutcome::Dropped`]（§4.8 精神同源：
//! 不静默、不 panic、供计数）；**不产生 `action_error`**——该面只对 client
//! 帧（`UNSUPPORTED_FRAME` 由 gateway 检查，§6）。
//!
//! 字段提取约定【决策】：ACP 线格式字段名文档未逐项规范，本模块优先
//! camelCase（`sessionId`/`turnId`/`entryId`/`blockId`/`toolCallId`/
//! `permissionId`/`createdAt`），回退 snake_case（兼容 `{type,payload}` 历史
//! 形态）。

use std::time::Duration;

use chrono::DateTime;
use serde_json::Value;

use acp_hub_proto::action::PermissionDecision;
use acp_hub_proto::schema::{BlockVisibility, ChatStatus, PublicError, ToolCallStatus, TurnStatus};

use crate::state::normalized::{EventBody, NormalizedEvent, PermissionToolSnapshot};

/// 权限请求超时（§16/§7.1：5min，`expires_at` 由 server 权威时钟注入，§4.7）。
pub const PERMISSION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 事件正文/错误消息端到端长度上限（§9.3：4KB 截断是容量约束；脱敏先于
/// 截断——本层只提取结构化字段，自由文本仅 message 类，截断处理见
/// [`truncate_text`]）。
pub const TEXT_MAX_BYTES: usize = 4096;

/// 官方 `session/request_permission` request 解析产物（#1 权限机制官方化）。
///
/// agent→client 的 JSON-RPC **request**（带 id，须回响应）；字段按官方
/// schema v1 提取（params = `{sessionId, toolCall, options}`）。
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequestFields {
    /// agent 的 request id（原样，响应帧 id 回显；JSON-RPC id 可为
    /// string/number——`as_str` 失败不得丢弃，与 RpcResponse 分支的关键
    /// 差异）。
    pub request_id: serde_json::Value,
    /// server 生成的 permission_id（uuid v4；聚合器投影键 §5.4）。
    pub permission_id: String,
    /// toolCall.toolCallId（透传 → 投影 tool_call_id）。
    pub tool_call_id: Option<String>,
    /// 官方 request 自带完整 toolCall；保留 rawInput，使 permission-first
    /// 顺序仍能原子投影可理解、可审计的工具卡。
    pub tool: PermissionToolSnapshot,
    pub title: String,
    /// 官方无 description 字段 → None。
    pub description: Option<String>,
    /// 官方 options 原样（`{optionId,name,kind}` 数组；响应须回显 optionId）。
    pub options: Vec<serde_json::Value>,
    /// 官方 params.sessionId（acp_session_id，binding 已校验；仅 relay
    /// register 用，不写入 EventBody——PendingPermissionReq.chat_id 已承载
    /// 归属，relay 侧不落表）。
    pub session_id: String,
}

/// 规范化结果（§6.1 事件表 + RpcResponse 专门面）。
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizeOutcome {
    /// 业务事件（投递 DocManager 聚合；Box 消弭与 RpcResponse 的变体大小
    /// 差异——NormalizedEvent 240B vs 响应面 ~40B）。
    Event(Box<NormalizedEvent>),
    /// JSON-RPC response（`id` 匹配 pending_rpc → L3 确认，§4.4；不产生业务
    /// 事件）。`is_error` 区分成功/错误响应。
    RpcResponse {
        /// 响应 id（rpc_id）。
        id: String,
        /// 是否 JSON-RPC error 响应。
        is_error: bool,
    },
    /// 官方 `session/request_permission` request（agent→client，带 id，须回
    /// 响应；#1 权限机制官方化）。由 relay 登记 pending_permissions 表并
    /// 投递 `PermissionRequested` 事件，coordinator resolve 时回官方响应帧。
    PermissionRequest(Box<PermissionRequestFields>),
    /// 丢弃 + 原因（调用方计数，不 panic 不静默，§4.8 精神）。
    Dropped(DropReason),
}

/// 丢弃原因（§3 映射表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// 双格式 sessionId 均缺失（上层按 NoSessionId 丢弃并计数）。
    NoSessionId,
    /// 缺少必要关联信息（无 turn_id 的增量等，§6.3 同源拒绝）。
    MissingField,
    /// 帧结构非法（非对象、payload 非对象）。
    Malformed,
    /// 未知 type / JSON-RPC method（§4.8 白名单精神；不静默、不 panic）。
    UnsupportedFrame,
}

impl DropReason {
    /// 稳定计数 key（脱敏、可聚合，§17.1）。
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::NoSessionId => "no_session_id",
            DropReason::MissingField => "missing_field",
            DropReason::Malformed => "malformed",
            DropReason::UnsupportedFrame => "unsupported_frame",
        }
    }
}

/// 入站规范化器（§6.1）。无内部状态（可并发共享）；`permission_timeout`
/// 决定 `permission_request` 的 `expires_at` 注入。
#[derive(Debug, Clone, Copy)]
pub struct AcpChannel {
    /// 权限请求超时（§16 默认 5min）。
    pub permission_timeout: Duration,
}

impl Default for AcpChannel {
    fn default() -> Self {
        AcpChannel {
            permission_timeout: PERMISSION_TIMEOUT,
        }
    }
}

impl AcpChannel {
    /// 主入口：原始 ACP 帧 → 规范化事件（§6.1）。
    ///
    /// `chat_id` 为 **hub 侧** id（调用方已按 binding 翻译与校验）；`epoch`/
    /// `seq` 为 instance 侧流纪元与单调序号（§4.5.1，透传进 NormalizedEvent
    /// envelope）；`now_rfc3339` 为 server 权威时钟（§4.7——permission
    /// expires_at 判定性时间戳由 server 生成，instance 只上报相对时序）。
    pub fn normalize(
        &self,
        chat_id: &str,
        epoch: u64,
        seq: u64,
        now_rfc3339: &str,
        frame: &Value,
    ) -> NormalizeOutcome {
        // 1. JSON-RPC 形态判定：有 "jsonrpc" 键 → 通知（method）/ response（id）。
        if frame.get("jsonrpc").is_some() {
            return self.normalize_json_rpc(chat_id, epoch, seq, now_rfc3339, frame);
        }
        // 2. 原始 {type, payload} 形态。
        let Some(obj) = frame.as_object() else {
            return NormalizeOutcome::Dropped(DropReason::Malformed);
        };
        let Some(kind) = obj.get("type").and_then(Value::as_str) else {
            // 非对象 / 缺 type：sessionId 提取优先（上层丢弃语义按 NoSessionId
            // 分类），否则 Malformed。
            if extract_session_id(frame).is_none() {
                return NormalizeOutcome::Dropped(DropReason::Malformed);
            }
            return NormalizeOutcome::Dropped(DropReason::MissingField);
        };
        let payload = match obj.get("payload") {
            Some(Value::Object(p)) => p.clone(),
            Some(_) => return NormalizeOutcome::Dropped(DropReason::Malformed),
            None => serde_json::Map::new(),
        };
        match self.map_raw(kind, &payload, now_rfc3339) {
            Ok(body) => NormalizeOutcome::Event(Box::new(NormalizedEvent {
                chat_id: chat_id.to_string(),
                seq,
                epoch,
                ts: now_rfc3339.to_string(),
                body,
            })),
            Err(MapError::Unsupported) => NormalizeOutcome::Dropped(DropReason::UnsupportedFrame),
            Err(MapError::MissingField) => NormalizeOutcome::Dropped(DropReason::MissingField),
        }
    }

    /// JSON-RPC 形态（§6.1 包裹格式）。
    fn normalize_json_rpc(
        &self,
        chat_id: &str,
        epoch: u64,
        seq: u64,
        now_rfc3339: &str,
        frame: &Value,
    ) -> NormalizeOutcome {
        let Some(obj) = frame.as_object() else {
            return NormalizeOutcome::Dropped(DropReason::Malformed);
        };
        // response：有 id、无 method。
        if obj.contains_key("id") && !obj.contains_key("method") {
            let Some(id) = obj.get("id").and_then(Value::as_str) else {
                return NormalizeOutcome::Dropped(DropReason::Malformed);
            };
            let is_error = obj.get("error").is_some();
            return NormalizeOutcome::RpcResponse {
                id: id.to_string(),
                is_error,
            };
        }
        // notification：method。
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return NormalizeOutcome::Dropped(DropReason::Malformed);
        };
        let params = match obj.get("params") {
            Some(Value::Object(p)) => p.clone(),
            Some(_) => return NormalizeOutcome::Dropped(DropReason::Malformed),
            None => serde_json::Map::new(),
        };
        // session/update 通知：params 两种形态——
        //   a) ACP 包裹 `{type, payload}`（acp-hub 私有帧）；
        //   b) agent-client-protocol `{sessionId, update: {sessionUpdate, ...}}`
        //      （真实 peri 实测；照抄 @fenix/chat-channel acp-channel.ts 映射）。
        if method == "session/update" {
            if let Some(update) = params.get("update").and_then(|v| v.as_object()) {
                if update.get("sessionUpdate").is_some() {
                    return match self.map_acp_update(update, now_rfc3339) {
                        Ok(body) => NormalizeOutcome::Event(Box::new(NormalizedEvent {
                            chat_id: chat_id.to_string(),
                            seq,
                            epoch,
                            ts: now_rfc3339.to_string(),
                            body,
                        })),
                        Err(MapError::Unsupported) => {
                            NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
                        }
                        Err(MapError::MissingField) => {
                            NormalizeOutcome::Dropped(DropReason::MissingField)
                        }
                    };
                }
            }
            let Some(kind) = params.get("type").and_then(Value::as_str) else {
                return NormalizeOutcome::Dropped(DropReason::MissingField);
            };
            let payload = match params.get("payload") {
                Some(Value::Object(p)) => p.clone(),
                Some(_) => return NormalizeOutcome::Dropped(DropReason::Malformed),
                None => serde_json::Map::new(),
            };
            return match self.map_raw(kind, &payload, now_rfc3339) {
                Ok(body) => NormalizeOutcome::Event(Box::new(NormalizedEvent {
                    chat_id: chat_id.to_string(),
                    seq,
                    epoch,
                    ts: now_rfc3339.to_string(),
                    body,
                })),
                Err(MapError::Unsupported) => {
                    NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
                }
                Err(MapError::MissingField) => NormalizeOutcome::Dropped(DropReason::MissingField),
            };
        }
        // agent 状态通知（`agent/status`）。
        if method == "agent/status" {
            return NormalizeOutcome::Event(Box::new(NormalizedEvent {
                chat_id: chat_id.to_string(),
                seq,
                epoch,
                ts: now_rfc3339.to_string(),
                body: EventBody::AgentStatus {
                    status: string_field(&params, "status", "status").unwrap_or_default(),
                    public_error: public_error(&params),
                    // 模型/上下文（跨任务契约 §1）：缺省 None（不覆盖 agent map）。
                    model: string_field(&params, "model", "model"),
                    context_window: number_field(&params, "contextWindow", "context_window"),
                    context_used: number_field(&params, "contextUsed", "context_used"),
                },
            }));
        }
        // 官方 `session/request_permission` request（#1 权限机制官方化，
        // schema v1）：agent→client 请求权限。带 id（须回响应，§4.4 响应
        // 帧无回执、以 forward_ack 为确认点）；params 必含 sessionId/
        // toolCall/options。
        if method == "session/request_permission" {
            let Some(id) = obj.get("id").filter(|v| !v.is_null()) else {
                // 无 id → 无法回响应（官方为 request 形态，非 notification）。
                return NormalizeOutcome::Dropped(DropReason::MissingField);
            };
            return match self.normalize_request_permission(id, &params) {
                Ok(req) => NormalizeOutcome::PermissionRequest(Box::new(req)),
                Err(MapError::MissingField) => NormalizeOutcome::Dropped(DropReason::MissingField),
                Err(MapError::Unsupported) => {
                    NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
                }
            };
        }
        NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
    }

    /// 官方 `session/request_permission` 解析（schema v1）：params =
    /// `{sessionId(req), toolCall(req, ToolCallUpdate), options(req)}`。
    ///
    /// - `permission_id` 由 server 生成（uuid v4，§4.7 server 权威；官方
    ///   request 无 permissionId 字段）；
    /// - `title` = toolCall.title → toolCall.toolCallId → 空串回退；
    /// - `description` 官方无字段 → None；
    /// - `request_id` 帧顶层 id **原样保留** `serde_json::Value`
    ///   （string/number 均合法；`as_str` 失败不得丢弃——这是与现有
    ///   [`NormalizeOutcome::RpcResponse`] 分支 160-162 的关键差异）。
    fn normalize_request_permission(
        &self,
        request_id: &Value,
        params: &serde_json::Map<String, Value>,
    ) -> Result<PermissionRequestFields, MapError> {
        let session_id = required(params, "sessionId", "session_id")?;
        let tool_call = params
            .get("toolCall")
            .and_then(Value::as_object)
            .ok_or(MapError::MissingField)?;
        let tool_call_id =
            string_field(tool_call, "toolCallId", "tool_call_id").ok_or(MapError::MissingField)?;
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .ok_or(MapError::MissingField)?;
        for o in options {
            let obj = o.as_object().ok_or(MapError::MissingField)?;
            if string_field(obj, "optionId", "option_id").is_none() {
                return Err(MapError::MissingField);
            }
        }
        let title =
            string_field(tool_call, "title", "title").unwrap_or_else(|| tool_call_id.clone());
        Ok(PermissionRequestFields {
            request_id: request_id.clone(),
            permission_id: uuid::Uuid::new_v4().to_string(),
            tool_call_id: Some(tool_call_id.clone()),
            tool: PermissionToolSnapshot {
                tool_call_id,
                name: title.clone(),
                arguments: tool_call.get("rawInput").cloned(),
            },
            title,
            description: None,
            options: options.clone(),
            session_id,
        })
    }

    /// agent-client-protocol `session/update` 的 `update` 对象 → EventBody。
    ///
    /// 照抄 @fenix/chat-channel `protocol/acp-channel.ts`（mapSessionUpdateType /
    /// resolveToolCallType / extractContent / extractToolCallId 语义）：
    /// 真实 peri 的增量帧**无 turnId/entryId/blockId**，本层产出空 id，
    /// 由聚合器按 active_turn 归位（§7.2 宿主驱动 turn 模型）。
    fn map_acp_update(
        &self,
        update: &serde_json::Map<String, Value>,
        now_rfc3339: &str,
    ) -> Result<EventBody, MapError> {
        use EventBody as B;
        let kind =
            string_field(update, "sessionUpdate", "sessionUpdate").ok_or(MapError::MissingField)?;
        let content_text = || {
            // extractContent：优先 update.content，回退 update.text。
            update
                .get("content")
                .and_then(|v| v.get("text"))
                .and_then(Value::as_str)
                .map(truncate_text)
                .unwrap_or_else(|| {
                    truncate_text(&string_field(update, "text", "text").unwrap_or_default())
                })
        };
        let body = match kind.as_str() {
            "agent_message_chunk" => B::MessageDelta {
                turn_id: String::new(),
                entry_id: String::new(),
                block_id: String::new(),
                text: content_text(),
            },
            "agent_thought_chunk" => B::ReasoningDelta {
                turn_id: String::new(),
                entry_id: String::new(),
                block_id: String::new(),
                text: content_text(),
                visibility: BlockVisibility::Summary,
            },
            "user_message_chunk" => B::UserMessage {
                turn_id: String::new(),
                entry_id: String::new(),
                text: content_text(),
                author_user_id: None,
                created_at: now_rfc3339.to_string(),
            },
            // tool_call / tool_call_update 按 status 细分终态
            // （resolveToolCallType：running→started；completed/complete/done→
            // completed；failed/error→failed；缺省 running。官方 ToolCallStatus
            // 值域 pending/in_progress/completed/failed（#2），兼容别名
            // complete/done/error；pending/in_progress 归入 started 非终态）。
            "tool_call" | "tool_call_update" => {
                let tool_call_id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        update
                            .get("content")
                            .and_then(|v| v.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                if tool_call_id.is_empty() {
                    return Err(MapError::MissingField);
                }
                match string_field(update, "status", "status").as_deref() {
                    Some("completed") | Some("complete") | Some("done") => B::ToolCallCompleted {
                        turn_id: String::new(),
                        tool_call_id,
                        result: update
                            .get("rawOutput")
                            .or_else(|| update.get("output"))
                            .cloned(),
                        public_error: None,
                        completed_at: now_rfc3339.to_string(),
                    },
                    // #2：官方 failed 终态（ToolCallStatus 值域
                    // pending/in_progress/completed/failed）；error 为兼容
                    // 别名（同为 ToolCallCompleted + public_error）。
                    Some("failed") | Some("error") => B::ToolCallCompleted {
                        turn_id: String::new(),
                        tool_call_id,
                        result: None,
                        public_error: Some(PublicError {
                            code: "agent_error".to_string(),
                            message: string_field(update, "title", "title")
                                .unwrap_or_else(|| "Tool call failed".to_string()),
                        }),
                        completed_at: now_rfc3339.to_string(),
                    },
                    status => {
                        let status = nonterminal_tool_status(status);
                        if kind == "tool_call_update" {
                            B::ToolCallUpdated {
                                turn_id: String::new(),
                                tool_call_id,
                                status: Some(status),
                                arguments: update.get("rawInput").cloned(),
                            }
                        } else {
                            B::ToolCallStarted {
                                turn_id: String::new(),
                                tool_call_id,
                                name: string_field(update, "title", "title")
                                    .or_else(|| string_field(update, "name", "name"))
                                    .unwrap_or_default(),
                                status,
                                arguments: update.get("rawInput").cloned(),
                                created_at: now_rfc3339.to_string(),
                            }
                        }
                    }
                }
            }
            // session 元信息（title 等；peri 实测仅 updatedAt，其余缺省不覆盖）。
            "session_info_update" | "session_update" => B::SessionInfo {
                title: string_field(update, "title", "title"),
                status: None,
                active_turn_id: None,
            },
            // 模型/effort 配置（跨任务契约）：configOptions 中 id 为 model /
            // thinking_effort 的 option；任一缺失对应字段 None（部分更新，
            // 同 SessionInfo 语义）。model 取 options 匹配项（value ==
            // currentValue）的 name 括号内模型名（`alias (模型名)`），无括号
            // 回退整个 name。wire 字段名（schema v1.4.0）：SessionConfigSelect
            // = currentValue/options，SessionConfigSelectOption = value/name。
            "config_option_update" => {
                let options = update
                    .get("configOptions")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let (model, effort) = extract_agent_config(options);
                B::AgentConfig { model, effort }
            }
            // 上下文用量快照（跨任务契约）：used/size 必填（缺 → MissingField，
            // 与 tool_call_id 同源拒绝，§6.3）。
            "usage_update" => B::AgentUsage {
                context_window: number_field(update, "size", "size")
                    .ok_or(MapError::MissingField)?,
                context_used: number_field(update, "used", "used").ok_or(MapError::MissingField)?,
            },
            // M1 无需投影的会话级元数据（命令菜单/模式/计划）。
            "available_commands_update"
            | "current_mode_update"
            | "plan"
            | "plan_update"
            | "plan_removed" => {
                return Err(MapError::Unsupported);
            }
            _ => return Err(MapError::Unsupported),
        };
        Ok(body)
    }

    /// §6.1 事件映射表核心：`{type,payload}` → EventBody（14 变体）。
    ///
    /// 私有错误收敛为 [`MapError`]（调用方统一转为
    /// [`DropReason::MissingField`]，§6.3 同源拒绝）。
    fn map_raw(
        &self,
        kind: &str,
        payload: &serde_json::Map<String, Value>,
        now_rfc3339: &str,
    ) -> Result<EventBody, MapError> {
        use EventBody as B;
        let body = match kind {
            // ---- 文本 / 推理增量（§5.3）----
            "agent_message_chunk" => B::MessageDelta {
                turn_id: required(payload, "turnId", "turn_id")?,
                entry_id: required(payload, "entryId", "entry_id")?,
                block_id: required(payload, "blockId", "block_id")?,
                text: truncate_text(&string_field(payload, "text", "text").unwrap_or_default()),
            },
            // 架构 §6.1 表为 agent_thought_chunk；任务描述 agent_reasoning_chunk
            // 为别名（冲突裁决：以架构为准，两者同映射，f5 设计稿 §3.2）。
            "agent_thought_chunk" | "agent_reasoning_chunk" => {
                let visibility = match string_field(payload, "visibility", "visibility").as_deref()
                {
                    Some("hidden") => BlockVisibility::Hidden,
                    _ => BlockVisibility::Summary,
                };
                B::ReasoningDelta {
                    turn_id: required(payload, "turnId", "turn_id")?,
                    entry_id: required(payload, "entryId", "entry_id")?,
                    block_id: required(payload, "blockId", "block_id")?,
                    text: truncate_text(&string_field(payload, "text", "text").unwrap_or_default()),
                    visibility,
                }
            }
            // ---- 用户消息（服务端单写注册映射，§6.5；幂等以 turn_id）----
            "user_message_chunk" => B::UserMessage {
                turn_id: required(payload, "turnId", "turn_id")?,
                entry_id: required(payload, "entryId", "entry_id")?,
                text: truncate_text(&string_field(payload, "text", "text").unwrap_or_default()),
                author_user_id: string_field(payload, "authorUserId", "author_user_id"),
                created_at: string_field(payload, "createdAt", "created_at")
                    .unwrap_or_else(|| now_rfc3339.to_string()),
            },
            // ---- Turn 终态（§7.2）----
            "prompt_complete" | "agent_message_complete" => B::TurnTerminal {
                turn_id: required(payload, "turnId", "turn_id")?,
                status: TurnStatus::Completed,
                completed_at: now_rfc3339.to_string(),
                public_error: None,
            },
            "turn_cancelled" => B::TurnTerminal {
                turn_id: required(payload, "turnId", "turn_id")?,
                status: TurnStatus::Cancelled,
                completed_at: now_rfc3339.to_string(),
                public_error: None,
            },
            "session_error" => B::TurnTerminal {
                turn_id: required(payload, "turnId", "turn_id")?,
                status: TurnStatus::Failed,
                completed_at: now_rfc3339.to_string(),
                public_error: public_error(payload),
            },
            // ---- 工具调用（§5.3 tool_calls）----
            "tool_call" => B::ToolCallStarted {
                turn_id: required(payload, "turnId", "turn_id")?,
                tool_call_id: required(payload, "toolCallId", "tool_call_id")?,
                name: string_field(payload, "name", "name").unwrap_or_default(),
                status: nonterminal_tool_status(
                    string_field(payload, "status", "status").as_deref(),
                ),
                arguments: opt_json(payload, "arguments"),
                created_at: string_field(payload, "createdAt", "created_at")
                    .unwrap_or_else(|| now_rfc3339.to_string()),
            },
            "tool_call_update" => {
                let status = string_field(payload, "status", "status").unwrap_or_default();
                let tool_call_id = required(payload, "toolCallId", "tool_call_id")?;
                if matches!(status.as_str(), "completed" | "error" | "failed") {
                    B::ToolCallCompleted {
                        turn_id: string_field(payload, "turnId", "turn_id").unwrap_or_default(),
                        tool_call_id,
                        result: opt_json(payload, "result"),
                        public_error: public_error(payload),
                        completed_at: now_rfc3339.to_string(),
                    }
                } else {
                    // running / streaming / 其余：M1 arguments 全量覆盖（§6.1 表）。
                    B::ToolCallUpdated {
                        turn_id: string_field(payload, "turnId", "turn_id").unwrap_or_default(),
                        tool_call_id,
                        status: Some(nonterminal_tool_status(Some(status.as_str()))),
                        arguments: opt_json(payload, "arguments"),
                    }
                }
            }
            // ---- 权限（§5.4 pending_permissions）----
            "permission_request" => {
                let expires_at = now_rfc3339.to_string();
                let expires_at = match DateTime::parse_from_rfc3339(now_rfc3339) {
                    Ok(t) => (t + chrono::Duration::from_std(self.permission_timeout)
                        .unwrap_or(chrono::Duration::seconds(300)))
                    .to_rfc3339(),
                    Err(_) => expires_at,
                };
                B::PermissionRequested {
                    permission_id: required(payload, "permissionId", "permission_id")?,
                    turn_id: required(payload, "turnId", "turn_id")?,
                    tool_call_id: string_field(payload, "toolCallId", "tool_call_id"),
                    tool: None,
                    title: string_field(payload, "title", "title").unwrap_or_default(),
                    description: string_field(payload, "description", "description"),
                    options: permission_options(payload),
                    expires_at,
                }
            }
            "permission_response" => B::PermissionResolved {
                permission_id: required(payload, "permissionId", "permission_id")?,
                decision: match string_field(payload, "decision", "decision").as_deref() {
                    // M1 决议面只有 allow/deny（§4.3）；allow_once/allow_session
                    // 归一为 allow（会话级档位后置）。
                    Some("allow") | Some("allow_once") | Some("allow_session") => {
                        PermissionDecision::Allow
                    }
                    Some("deny") => PermissionDecision::Deny,
                    _ => return Err(MapError::MissingField),
                },
            },
            // ---- Session 元信息 / 能力（§5.4，部分更新）----
            "session_update" => B::SessionInfo {
                title: string_field(payload, "title", "title"),
                status: string_field(payload, "status", "status")
                    .as_deref()
                    .and_then(|s| match s {
                        "accepting" => Some(ChatStatus::Accepting),
                        "active" => Some(ChatStatus::Active),
                        "ended" => Some(ChatStatus::Ended),
                        "closed" => Some(ChatStatus::Closed),
                        "crashed" => Some(ChatStatus::Crashed),
                        _ => None,
                    }),
                active_turn_id: string_field(payload, "activeTurnId", "active_turn_id"),
            },
            "available_commands_update" => B::Capabilities {
                capabilities: payload
                    .get("commands")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            // ---- Agent 状态 / session_list（§5.4）----
            "agent_status" => {
                return Ok(EventBody::AgentStatus {
                    status: string_field(payload, "status", "status").unwrap_or_default(),
                    public_error: public_error(payload),
                    model: string_field(payload, "model", "model"),
                    context_window: number_field(payload, "contextWindow", "context_window"),
                    context_used: number_field(payload, "contextUsed", "context_used"),
                })
            }
            "session_list" => {
                let entries = payload
                    .get("sessions")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| {
                                let o = v.as_object()?;
                                let id = string_field(o, "sessionId", "session_id")?;
                                let title = string_field(o, "title", "title").unwrap_or_default();
                                let status =
                                    string_field(o, "status", "status").unwrap_or_default();
                                let updated_at =
                                    string_field(o, "updatedAt", "updated_at").unwrap_or_default();
                                Some(acp_hub_proto::schema::SessionSummaryProjection {
                                    session_id: id,
                                    title,
                                    status,
                                    updated_at,
                                    // control doc 侧无 cwd 面（poller 直连路径
                                    // 由轮询侧标注，§6.3 workspace 扩展）。
                                    cwd: String::new(),
                                    bound_chat_id: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                B::SessionListResponse { entries }
            }
            _ => return Err(MapError::Unsupported),
        };
        Ok(body)
    }
}

/// 双格式 sessionId 提取（§3.3/§6.1 兼容规则；提取到的是原始 acp_session_id）。
///
/// 1. 原始 `{type, payload}`：`payload.sessionId` → `payload.session_id` →
///    顶层 `sessionId`；
/// 2. JSON-RPC 包裹：`params.sessionId` → `params.session_id`（notification
///    与 response 同规则）；
/// 3. 均缺失 → None（上层按 [`DropReason::NoSessionId`] 丢弃并计数）。
pub fn extract_session_id(frame: &Value) -> Option<String> {
    let obj = frame.as_object()?;
    // 原始形态：payload 内优先。
    if let Some(payload) = obj.get("payload").and_then(Value::as_object) {
        if let Some(id) = field(payload, &["sessionId", "session_id"]) {
            return Some(id);
        }
    }
    if let Some(id) = field(obj, &["sessionId", "session_id"]) {
        return Some(id);
    }
    // JSON-RPC 形态：params 内。
    if let Some(params) = obj.get("params").and_then(Value::as_object) {
        if let Some(id) = field(params, &["sessionId", "session_id"]) {
            return Some(id);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 字段提取 helper（camelCase 优先，snake_case 回退）
// ---------------------------------------------------------------------------

fn field(obj: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| obj.get(*n).and_then(Value::as_str).map(str::to_string))
}

fn string_field(obj: &serde_json::Map<String, Value>, camel: &str, snake: &str) -> Option<String> {
    field(obj, &[camel, snake])
}

/// Normalize ACP's non-terminal aliases. Terminal values are handled by callers before this
/// helper; unknown or absent values remain pending for backward compatibility.
fn nonterminal_tool_status(status: Option<&str>) -> ToolCallStatus {
    match status {
        Some("in_progress" | "running" | "streaming") => ToolCallStatus::Running,
        Some("awaiting_permission" | "awaitingPermission") => ToolCallStatus::AwaitingPermission,
        _ => ToolCallStatus::Pending,
    }
}

/// 非负整数提取（camelCase 优先，snake_case 回退）：负数/超 u32 上限 →
/// None（缺省语义，不整体拒绝——§6.3 仅必填字段缺失才 MissingField）。
fn number_field(obj: &serde_json::Map<String, Value>, camel: &str, snake: &str) -> Option<u32> {
    [camel, snake]
        .iter()
        .find_map(|n| obj.get(*n).and_then(Value::as_u64))
        .and_then(|n| u32::try_from(n).ok())
}

/// 必填字符串字段：缺失 → 整体 [`DropReason::MissingField`]（§6.3 同源拒绝）。
fn required(
    obj: &serde_json::Map<String, Value>,
    camel: &str,
    snake: &str,
) -> Result<String, MapError> {
    string_field(obj, camel, snake).ok_or(MapError::MissingField)
}

fn opt_json(obj: &serde_json::Map<String, Value>, name: &str) -> Option<Value> {
    obj.get(name).cloned()
}

/// map_raw 内部错误（私有；调用方统一收敛为 [`DropReason`]，与外部
/// [`NormalizeOutcome`] 解耦——避免大 Err 变体；语义由收敛点精确映射，§6.3
/// 同源拒绝）。
#[derive(Debug, Clone, Copy)]
enum MapError {
    /// 缺少必要关联信息（无 turn_id 的增量等，§6.3 同源拒绝）。
    MissingField,
    /// 未知 type（§4.8 白名单精神：不静默、不 panic、供计数）。
    Unsupported,
}

/// public_error 提取（§9.3：稳定错误码 + 脱敏消息；message 截断 4KB）。
fn public_error(obj: &serde_json::Map<String, Value>) -> Option<PublicError> {
    let err = obj.get("publicError").or_else(|| obj.get("public_error"))?;
    let o = err.as_object()?;
    let code = string_field(o, "code", "code").unwrap_or_else(|| "AGENT_ERROR".to_string());
    let message = string_field(o, "message", "message").unwrap_or_default();
    Some(PublicError {
        code,
        message: truncate_text(&message),
    })
}

fn permission_options(
    obj: &serde_json::Map<String, Value>,
) -> Vec<acp_hub_proto::schema::PermissionOptions> {
    use acp_hub_proto::schema::PermissionOptions as O;
    obj.get("options")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some(match v.as_str()? {
                        "allow_once" | "allowOnce" => O::AllowOnce,
                        "allow_session" | "allowSession" => O::AllowSession,
                        "deny" => O::Deny,
                        _ => return None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 脱敏截断（§9.3：4KB 容量约束；按字节截断，非法 UTF-8 边界由
/// `String::truncate` 语义规避——先按 char 边界裁剪）。
fn truncate_text(s: &str) -> String {
    if s.len() <= TEXT_MAX_BYTES {
        return s.to_string();
    }
    let mut end = TEXT_MAX_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 从 `alias (模型名)` 形式 label 提取括号内模型名（config_option_update，
/// 跨任务契约）；无括号/括号内为空 → 整个 label。
fn extract_model_name(label: &str) -> String {
    let Some(open) = label.rfind('(') else {
        return label.to_string();
    };
    let Some(rel) = label[open..].find(')') else {
        return label.to_string();
    };
    let inner = label[open + 1..open + rel].trim();
    if inner.is_empty() {
        label.to_string()
    } else {
        inner.to_string()
    }
}

/// 从 ACP `configOptions` 数组提取 `(model, effort)`（跨任务契约）：id 为
/// `model` 的 option → options 匹配项的 name 内模型名；id 为
/// `thinking_effort` 的 option → currentValue。任一缺失 → None（部分更新
/// 语义）。
///
/// wire 字段（agent-client-protocol schema v1）：option 顶层为
/// `currentValue`/`options`（flatten 的 SessionConfigSelect，camelCase），
/// options 元素为 `{ value, name }`。
///
/// 两条消费路径共用：`config_option_update` 通知（map_acp_update）与
/// session/new 响应体（coordinator，handle_new 不发通知、响应即唯一路径）。
pub fn extract_agent_config(options: &[serde_json::Value]) -> (Option<String>, Option<String>) {
    let model = options.iter().find_map(|o| {
        let o = o.as_object()?;
        if o.get("id").and_then(Value::as_str) != Some("model") {
            return None;
        }
        let current = o.get("currentValue").and_then(Value::as_str)?;
        let name = o
            .get("options")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|s| s.as_object())
            .find(|s| s.get("value").and_then(Value::as_str) == Some(current))?
            .get("name")
            .and_then(Value::as_str)?;
        Some(extract_model_name(name))
    });
    let effort = options.iter().find_map(|o| {
        let o = o.as_object()?;
        if o.get("id").and_then(Value::as_str) != Some("thinking_effort") {
            return None;
        }
        o.get("currentValue")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    (model, effort)
}

#[cfg(test)]
#[path = "acp_channel_test.rs"]
mod acp_channel_test;
