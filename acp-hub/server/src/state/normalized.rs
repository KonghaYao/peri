//! 规范化事件（§6.1）：ACPChannel 产物的统一形态。
//!
//! 由 state 层定义、供 F5 ACPChannel 产出（proto `event.rs` 的
//! `EventFrame.frame` 是此类型的 serde 投影——`events/subscribe` 推送不透明
//! JSON，结构以本类型为准）。

use serde::{Deserialize, Serialize};

use acp_hub_proto::action::PermissionDecision;
use acp_hub_proto::schema::{
    BlockVisibility, ChatStatus, PermissionOptions, PublicError, SessionSummaryProjection,
    ToolCallStatus, TurnStatus,
};

/// 规范化事件（§6.1）：ACPChannel 产物的统一形态。
///
/// envelope 携带路由与重放序依据：`(chat_id, epoch, seq)` 是终态守卫
/// （§6.3）与 gap 计数（§8.5）的输入；body 只含业务字段。事件按 chat
/// 路由到对应写者，聚合器校验 envelope.chat_id 与自身 chat 一致（防串）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedEvent {
    /// hub 侧 chat_id（经 binding 翻译，非原始 ACP 会话 session_id）。
    ///
    /// wire 信封字段名保持 `sessionId`（§4.5.1 信封契约：instance 透传
    /// 帧以 `sessionId` 标记归属，其值即 server 下发的 chat_id 标签）。
    #[serde(rename = "sessionId")]
    pub chat_id: String,
    /// instance 侧单调 seq（同 epoch 内；§4.5.1）。
    pub seq: u64,
    /// stream_epoch（instance 侧流代际标识；epoch 变化 → 不可校准缺口）。
    pub epoch: u64,
    /// server 权威时钟（§4.7；RFC3339，normalize 时注入）。聚合器用它做
    /// 回放合成（§8.5 REPLAY_NEEDS_TURN 占位 user 消息）等需要事件时刻的
    /// 投影；`#[serde(default)]` 兼容旧 update 日志（缺 ts 视为空）。
    #[serde(default)]
    pub ts: String,
    pub body: EventBody,
}

/// Tool facts carried by an official permission request.
///
/// ACP permits `session/request_permission` to arrive before the matching
/// `tool_call` notification. Keeping this snapshot on the same normalized
/// event lets the single writer create the tool card and permission prompt
/// atomically without inventing a second stream sequence number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionToolSnapshot {
    pub tool_call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
}

impl NormalizedEvent {
    /// 事件种类标识（脱敏日志用；不暴露正文/参数）。
    pub fn kind(&self) -> &'static str {
        self.body.kind()
    }
}

/// 事件体（§6.1 事件表全子集）。serde tag `"type"`（对齐 ACP `{type, payload}`
/// 习惯；事件类型名为 snake_case）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    /// 文本增量 → Chat Doc entry block（Y.Text 追加；微批次合并，§6.4）。
    MessageDelta {
        turn_id: String,
        entry_id: String,
        block_id: String,
        text: String,
    },
    /// 思考/推理增量 → Chat Doc reasoning block；按可见性写 summary/hidden，
    /// hidden 绝不发给无权客户端（§5.3）。
    ReasoningDelta {
        turn_id: String,
        entry_id: String,
        block_id: String,
        text: String,
        visibility: BlockVisibility,
    },
    /// 用户消息（服务端单写注册，§6.5）：`session/prompt` 处理时注册 turnId 并
    /// 创建 user entry，ACP 的 `user_message_chunk` 以此映射。幂等：同 turn_id
    /// 重放跳过。
    UserMessage {
        turn_id: String,
        entry_id: String,
        text: String,
        author_user_id: Option<String>,
        created_at: String,
    },
    /// 工具调用开始 → Chat Doc tool_calls（按 tool_call_id 创建，幂等）。
    ToolCallStarted {
        turn_id: String,
        tool_call_id: String,
        name: String,
        /// ACP-observed initial lifecycle state. Legacy producers default to pending.
        #[serde(default = "pending_tool_status")]
        status: ToolCallStatus,
        arguments: Option<serde_json::Value>,
        created_at: String,
    },
    /// 工具调用更新 → tool_calls upsert（M1：arguments 全量覆盖；状态按服务端
    /// 单调状态机迁移）。
    ToolCallUpdated {
        turn_id: String,
        tool_call_id: String,
        /// None means an arguments-only legacy update.
        #[serde(default)]
        status: Option<ToolCallStatus>,
        arguments: Option<serde_json::Value>,
    },
    /// 工具调用完成 → tool_calls 状态迁移 Completed/Error（upsert）。
    /// 超大 result 仅保留受授权资源引用（截断策略见 §9.5）。
    ToolCallCompleted {
        turn_id: String,
        tool_call_id: String,
        result: Option<serde_json::Value>,
        public_error: Option<PublicError>,
        /// Hub observation time; paired with the start observation for UI duration.
        completed_at: String,
    },
    /// 权限请求 → Control Doc pending_permissions（按 permission_id upsert）。
    PermissionRequested {
        permission_id: String,
        turn_id: String,
        tool_call_id: Option<String>,
        /// Complete tool facts from official ACP requests. Legacy producers
        /// omit this field and retain the historical link-only behavior.
        #[serde(default)]
        tool: Option<PermissionToolSnapshot>,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOptions>,
        /// server 权威时钟（§4.7）；RFC3339。
        expires_at: String,
    },
    /// 权限解决 → pending_permissions CAS：仅 pending → resolved 原子迁移一次
    /// （§7.4 规则 4），迁移成功后 decision 写入；重复回答幂等返回（§10）。
    PermissionResolved {
        permission_id: String,
        decision: PermissionDecision,
    },
    /// 权限过期 → pending → expired（CAS；decision 保持 null，§5.4）。
    /// 来源两条：ACP 事件流 / server 定时器（§4.7 判定性时间戳）——都落到
    /// 同一 CAS 原语。
    PermissionExpired { permission_id: String },
    /// Agent 状态覆盖 → Control Doc agent.status/public_error（§6.3）。
    /// 能力未确认前保持不可用（见 Capabilities）。
    AgentStatus {
        status: String,
        public_error: Option<PublicError>,
        /// 当前模型名（agent/status 通知可携带；None 不覆盖 agent map）。
        model: Option<String>,
        /// 上下文窗口大小（token 数）。
        context_window: Option<u32>,
        /// 当前已用上下文（token 数）。
        context_used: Option<u32>,
    },
    /// Agent 配置部分更新 → Control Doc agent.model/effort（`config_option_update`
    /// 通知；跨任务契约：None 不覆盖既有值，同 SessionInfo 语义）。
    AgentConfig {
        /// 当前模型名（从 configOptions 中 model 项的 options 匹配
        /// currentValue 的 name 提取，`alias (模型名)` 括号内；无括号回退
        /// 整个 name）。
        model: Option<String>,
        /// 当前 effort（low/medium/high/xhigh/max）。
        effort: Option<String>,
    },
    /// Agent 上下文用量快照 → Control Doc agent.context_window/context_used
    /// （`usage_update` 通知；每次 LLM 调用结束发送，全量覆盖）。
    AgentUsage {
        /// 上下文窗口大小（token 数）。
        context_window: u32,
        /// 当前已用上下文（token 数）。
        context_used: u32,
    },
    /// 能力声明覆盖 → Control Doc agent.capabilities。
    Capabilities { capabilities: Vec<String> },
    /// Session 元信息覆盖 → Control Doc chat（title/status/active_turn_id）。
    /// 字段均 Option：缺省字段不覆盖（部分更新）。
    SessionInfo {
        title: Option<String>,
        status: Option<ChatStatus>,
        active_turn_id: Option<String>,
    },
    /// `session_list` 响应 → Control Doc sessions（agent 磁盘历史，全量同步投影，
    /// §5.2 裁决：与 Registry 活跃会话语义不同、互不替代）。
    SessionListResponse {
        entries: Vec<SessionSummaryProjection>,
    },
    /// Turn 终态（completed/failed/cancelled/interrupted）→ Chat Doc entry 终态
    /// 迁移 + Control Doc active_turn 更新（§7.2）。终态立即写入；之后的同 turn
    /// 增量丢弃（interrupted 例外：带 envelope 重放序依据恰一次校准，§6.3）。
    TurnTerminal {
        turn_id: String,
        /// 【决策】取值限定终态四值（Completed|Failed|Cancelled|Interrupted，
        /// §7.2）；聚合器对非终态值按 `InvalidTerminalStatus` 拒绝（防御）。
        status: TurnStatus,
        /// RFC3339（server 权威时钟，§4.7）。
        completed_at: String,
        public_error: Option<PublicError>,
    },
}

fn pending_tool_status() -> ToolCallStatus {
    ToolCallStatus::Pending
}

impl EventBody {
    /// 事件种类标识（脱敏日志用；不暴露正文/参数）。
    pub fn kind(&self) -> &'static str {
        match self {
            EventBody::MessageDelta { .. } => "message_delta",
            EventBody::ReasoningDelta { .. } => "reasoning_delta",
            EventBody::UserMessage { .. } => "user_message",
            EventBody::ToolCallStarted { .. } => "tool_call_started",
            EventBody::ToolCallUpdated { .. } => "tool_call_updated",
            EventBody::ToolCallCompleted { .. } => "tool_call_completed",
            EventBody::PermissionRequested { .. } => "permission_requested",
            EventBody::PermissionResolved { .. } => "permission_resolved",
            EventBody::PermissionExpired { .. } => "permission_expired",
            EventBody::AgentStatus { .. } => "agent_status",
            EventBody::AgentConfig { .. } => "agent_config",
            EventBody::AgentUsage { .. } => "agent_usage",
            EventBody::Capabilities { .. } => "capabilities",
            EventBody::SessionInfo { .. } => "session_info",
            EventBody::SessionListResponse { .. } => "session_list_response",
            EventBody::TurnTerminal { .. } => "turn_terminal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serde_tag_and_envelope_shape() {
        let ev = NormalizedEvent {
            chat_id: "s1".into(),
            seq: 3,
            epoch: 1,
            ts: "2026-08-07T00:00:00Z".to_string(),
            body: EventBody::MessageDelta {
                turn_id: "t1".into(),
                entry_id: "t1:assistant".into(),
                block_id: "b1".into(),
                text: "hi".into(),
            },
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["sessionId"], json!("s1"));
        assert_eq!(v["seq"], json!(3));
        assert_eq!(v["epoch"], json!(1));
        assert_eq!(v["body"]["type"], json!("message_delta"));
        assert_eq!(v["body"]["turn_id"], json!("t1"));
        assert_eq!(v["body"]["block_id"], json!("b1"));
    }

    #[test]
    fn serde_roundtrip_all_variants() {
        let bodies = vec![
            EventBody::MessageDelta {
                turn_id: "t".into(),
                entry_id: "e".into(),
                block_id: "b".into(),
                text: "x".into(),
            },
            EventBody::ReasoningDelta {
                turn_id: "t".into(),
                entry_id: "e".into(),
                block_id: "b".into(),
                text: "x".into(),
                visibility: BlockVisibility::Hidden,
            },
            EventBody::UserMessage {
                turn_id: "t".into(),
                entry_id: "t:user".into(),
                text: "x".into(),
                author_user_id: None,
                created_at: "2026-08-07T00:00:00Z".into(),
            },
            EventBody::ToolCallStarted {
                turn_id: "t".into(),
                tool_call_id: "tc1".into(),
                name: "n".into(),
                status: ToolCallStatus::Pending,
                arguments: Some(json!({"a": 1})),
                created_at: "2026-08-07T00:00:00Z".into(),
            },
            EventBody::ToolCallUpdated {
                turn_id: "t".into(),
                tool_call_id: "tc1".into(),
                status: None,
                arguments: None,
            },
            EventBody::ToolCallCompleted {
                turn_id: "t".into(),
                tool_call_id: "tc1".into(),
                result: None,
                public_error: None,
                completed_at: "2026-08-07T00:00:01Z".into(),
            },
            EventBody::PermissionRequested {
                permission_id: "p1".into(),
                turn_id: "t".into(),
                tool_call_id: None,
                tool: None,
                title: "x".into(),
                description: None,
                options: vec![PermissionOptions::AllowOnce],
                expires_at: "2026-08-07T00:00:00Z".into(),
            },
            EventBody::PermissionResolved {
                permission_id: "p1".into(),
                decision: PermissionDecision::Allow,
            },
            EventBody::PermissionExpired {
                permission_id: "p1".into(),
            },
            EventBody::AgentStatus {
                status: "running".into(),
                public_error: None,
                model: Some("claude-sonnet-4-5".into()),
                context_window: Some(200_000),
                context_used: Some(42_000),
            },
            EventBody::AgentConfig {
                model: Some("claude-sonnet-4-5".into()),
                effort: Some("high".into()),
            },
            EventBody::AgentUsage {
                context_window: 200_000,
                context_used: 42_000,
            },
            EventBody::Capabilities {
                capabilities: vec!["ls".into()],
            },
            EventBody::SessionInfo {
                title: None,
                status: None,
                active_turn_id: None,
            },
            EventBody::SessionListResponse {
                entries: vec![SessionSummaryProjection {
                    session_id: "s".into(),
                    title: "x".into(),
                    status: "completed".into(),
                    updated_at: "2026-08-07T00:00:00Z".into(),
                    cwd: String::new(),
                    bound_chat_id: None,
                }],
            },
            EventBody::TurnTerminal {
                turn_id: "t".into(),
                status: TurnStatus::Completed,
                completed_at: "2026-08-07T00:00:00Z".into(),
                public_error: None,
            },
        ];
        for body in bodies {
            let s = serde_json::to_string(&body).unwrap();
            let back: EventBody = serde_json::from_str(&s).unwrap();
            assert_eq!(body, back, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn legacy_tool_events_default_missing_status_without_losing_arguments() {
        let started: EventBody = serde_json::from_value(json!({
            "type": "tool_call_started",
            "turn_id": "t1",
            "tool_call_id": "tc1",
            "name": "shell",
            "arguments": {"cmd": "pwd"},
            "created_at": "2026-08-07T00:00:00Z"
        }))
        .unwrap();
        assert!(matches!(
            started,
            EventBody::ToolCallStarted {
                status: ToolCallStatus::Pending,
                ..
            }
        ));

        let updated: EventBody = serde_json::from_value(json!({
            "type": "tool_call_updated",
            "turn_id": "t1",
            "tool_call_id": "tc1",
            "arguments": {"cmd": "pwd", "legacy": true}
        }))
        .unwrap();
        assert!(matches!(
            updated,
            EventBody::ToolCallUpdated { status: None, .. }
        ));
    }

    #[test]
    fn legacy_permission_event_defaults_missing_tool_snapshot() {
        let event: EventBody = serde_json::from_value(json!({
            "type": "permission_requested",
            "permission_id": "p1",
            "turn_id": "t1",
            "tool_call_id": "tc1",
            "title": "允许执行",
            "description": null,
            "options": ["allowOnce"],
            "expires_at": "2026-08-07T00:05:00Z"
        }))
        .unwrap();
        assert!(matches!(
            event,
            EventBody::PermissionRequested {
                tool: None,
                tool_call_id: Some(ref id),
                ..
            } if id == "tc1"
        ));
    }
}
