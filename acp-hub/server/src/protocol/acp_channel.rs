//! ACPChannel：入站规范化（架构 §6.1，设计稿 `f5-channel-control.md` §3）。
//!
//! machine 透传的原始 ACP 帧（`{type,payload}` 或 JSON-RPC `session/update`
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
use acp_hub_proto::schema::{BlockVisibility, PublicError, SessionStatus, TurnStatus};

use crate::state::normalized::{EventBody, NormalizedEvent};

/// 权限请求超时（§16/§7.1：5min，`expires_at` 由 server 权威时钟注入，§4.7）。
pub const PERMISSION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 事件正文/错误消息端到端长度上限（§9.3：4KB 截断是容量约束；脱敏先于
/// 截断——本层只提取结构化字段，自由文本仅 message 类，截断处理见
/// [`truncate_text`]）。
pub const TEXT_MAX_BYTES: usize = 4096;

/// 规范化结果（§6.1 事件表 + RpcResponse 专门面）。
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizeOutcome {
    /// 业务事件（投递 DocManager 聚合）。
    Event(NormalizedEvent),
    /// JSON-RPC response（`id` 匹配 pending_rpc → L3 确认，§4.4；不产生业务
    /// 事件）。`is_error` 区分成功/错误响应。
    RpcResponse {
        /// 响应 id（rpc_id）。
        id: String,
        /// 是否 JSON-RPC error 响应。
        is_error: bool,
    },
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
    /// `session_id` 为 **hub 侧** id（调用方已按 binding 翻译与校验）；`epoch`/
    /// `seq` 为 machine 侧流纪元与单调序号（§4.5.1，透传进 NormalizedEvent
    /// envelope）；`now_rfc3339` 为 server 权威时钟（§4.7——permission
    /// expires_at 判定性时间戳由 server 生成，machine 只上报相对时序）。
    pub fn normalize(
        &self,
        session_id: &str,
        epoch: u64,
        seq: u64,
        now_rfc3339: &str,
        frame: &Value,
    ) -> NormalizeOutcome {
        // 1. JSON-RPC 形态判定：有 "jsonrpc" 键 → 通知（method）/ response（id）。
        if frame.get("jsonrpc").is_some() {
            return self.normalize_json_rpc(session_id, epoch, seq, now_rfc3339, frame);
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
            Ok(body) => NormalizeOutcome::Event(NormalizedEvent {
                session_id: session_id.to_string(),
                seq,
                epoch,
                body,
            }),
            Err(MapError::Unsupported) => NormalizeOutcome::Dropped(DropReason::UnsupportedFrame),
            Err(MapError::MissingField) => NormalizeOutcome::Dropped(DropReason::MissingField),
        }
    }

    /// JSON-RPC 形态（§6.1 包裹格式）。
    fn normalize_json_rpc(
        &self,
        session_id: &str,
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
        // session/update 通知：params 含 {type, payload}（ACP 事件包裹）。
        if method == "session/update" {
            let Some(kind) = params.get("type").and_then(Value::as_str) else {
                return NormalizeOutcome::Dropped(DropReason::MissingField);
            };
            let payload = match params.get("payload") {
                Some(Value::Object(p)) => p.clone(),
                Some(_) => return NormalizeOutcome::Dropped(DropReason::Malformed),
                None => serde_json::Map::new(),
            };
            return match self.map_raw(kind, &payload, now_rfc3339) {
                Ok(body) => NormalizeOutcome::Event(NormalizedEvent {
                    session_id: session_id.to_string(),
                    seq,
                    epoch,
                    body,
                }),
                Err(MapError::Unsupported) => {
                    NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
                }
                Err(MapError::MissingField) => {
                    NormalizeOutcome::Dropped(DropReason::MissingField)
                }
            };        }
        // agent 状态通知（`agent/status`）。
        if method == "agent/status" {
            return NormalizeOutcome::Event(NormalizedEvent {
                session_id: session_id.to_string(),
                seq,
                epoch,
                body: EventBody::AgentStatus {
                    status: string_field(&params, "status", "status").unwrap_or_default(),
                    public_error: public_error(&params),
                },
            });
        }
        NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
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
                arguments: opt_json(payload, "arguments"),
                created_at: string_field(payload, "createdAt", "created_at")
                    .unwrap_or_else(|| now_rfc3339.to_string()),
            },
            "tool_call_update" => {
                let status = string_field(payload, "status", "status").unwrap_or_default();
                let tool_call_id = required(payload, "toolCallId", "tool_call_id")?;
                if matches!(status.as_str(), "completed" | "error" | "failed") {
                    B::ToolCallCompleted {
                        turn_id: string_field(payload, "turnId", "turn_id")
                            .unwrap_or_default(),
                        tool_call_id,
                        result: opt_json(payload, "result"),
                        public_error: public_error(payload),
                    }
                } else {
                    // running / streaming / 其余：M1 arguments 全量覆盖（§6.1 表）。
                    B::ToolCallUpdated {
                        turn_id: string_field(payload, "turnId", "turn_id").unwrap_or_default(),
                        tool_call_id,
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
                status: string_field(payload, "status", "status").as_deref().and_then(
                    |s| match s {
                        "accepting" => Some(SessionStatus::Accepting),
                        "active" => Some(SessionStatus::Active),
                        "ended" => Some(SessionStatus::Ended),
                        "closed" => Some(SessionStatus::Closed),
                        "crashed" => Some(SessionStatus::Crashed),
                        _ => None,
                    },
                ),
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
                                let title =
                                    string_field(o, "title", "title").unwrap_or_default();
                                let status =
                                    string_field(o, "status", "status").unwrap_or_default();
                                let updated_at = string_field(o, "updatedAt", "updated_at")
                                    .unwrap_or_default();
                                Some(acp_hub_proto::schema::SessionSummaryProjection {
                                    session_id: id,
                                    title,
                                    status,
                                    updated_at,
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

fn string_field(
    obj: &serde_json::Map<String, Value>,
    camel: &str,
    snake: &str,
) -> Option<String> {
    field(obj, &[camel, snake])
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

fn permission_options(obj: &serde_json::Map<String, Value>) -> Vec<acp_hub_proto::schema::PermissionOptions> {
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

#[cfg(test)]
#[path = "acp_channel_test.rs"]
mod acp_channel_test;
