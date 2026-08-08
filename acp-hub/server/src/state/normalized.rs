//! 规范化事件（§6.1）：ACPChannel 产物的统一形态。
//!
//! 由 state 层定义、供 F5 ACPChannel 产出（proto `event.rs` 的
//! `EventFrame.frame` 是此类型的 serde 投影——`events/subscribe` 推送不透明
//! JSON，结构以本类型为准）。

use serde::{Deserialize, Serialize};

use acp_hub_proto::action::PermissionDecision;
use acp_hub_proto::schema::{
    BlockVisibility, PermissionOptions, PublicError, SessionStatus, SessionSummaryProjection,
    TurnStatus,
};

/// 规范化事件（§6.1）：ACPChannel 产物的统一形态。
///
/// envelope 携带路由与重放序依据：`(session_id, epoch, seq)` 是终态守卫
/// （§6.3）与 gap 计数（§8.5）的输入；body 只含业务字段。事件按 session
/// 路由到对应写者，聚合器校验 envelope.session_id 与自身 session 一致（防串）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedEvent {
    /// hub 侧 session_id（经 binding 翻译，非原始 acp_session_id）。
    pub session_id: String,
    /// machine 侧单调 seq（同 epoch 内；§4.5.1）。
    pub seq: u64,
    /// stream_epoch（machine 侧流代际标识；epoch 变化 → 不可校准缺口）。
    pub epoch: u64,
    pub body: EventBody,
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
        arguments: Option<serde_json::Value>,
        created_at: String,
    },
    /// 工具调用更新 → tool_calls upsert（M1：arguments 全量覆盖；状态位不在此
    /// 迁移）。
    ToolCallUpdated {
        turn_id: String,
        tool_call_id: String,
        arguments: Option<serde_json::Value>,
    },
    /// 工具调用完成 → tool_calls 状态迁移 Completed/Error（upsert）。
    /// 超大 result 仅保留受授权资源引用（截断策略见 §9.5）。
    ToolCallCompleted {
        turn_id: String,
        tool_call_id: String,
        result: Option<serde_json::Value>,
        public_error: Option<PublicError>,
    },
    /// 权限请求 → Session Doc pending_permissions（按 permission_id upsert）。
    PermissionRequested {
        permission_id: String,
        turn_id: String,
        tool_call_id: Option<String>,
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
    /// Agent 状态覆盖 → Session Doc agent.status/public_error（§6.3）。
    /// 能力未确认前保持不可用（见 Capabilities）。
    AgentStatus {
        status: String,
        public_error: Option<PublicError>,
    },
    /// 能力声明覆盖 → Session Doc agent.capabilities。
    Capabilities { capabilities: Vec<String> },
    /// Session 元信息覆盖 → Session Doc session（title/status/active_turn_id）。
    /// 字段均 Option：缺省字段不覆盖（部分更新）。
    SessionInfo {
        title: Option<String>,
        status: Option<SessionStatus>,
        active_turn_id: Option<String>,
    },
    /// `session_list` 响应 → Session Doc sessions（agent 磁盘历史，全量同步投影，
    /// §5.2 裁决：与 Registry 活跃会话语义不同、互不替代）。
    SessionListResponse { entries: Vec<SessionSummaryProjection> },
    /// Turn 终态（completed/failed/cancelled/interrupted）→ Chat Doc entry 终态
    /// 迁移 + Session Doc active_turn 更新（§7.2）。终态立即写入；之后的同 turn
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
            session_id: "s1".into(),
            seq: 3,
            epoch: 1,
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
                arguments: Some(json!({"a": 1})),
                created_at: "2026-08-07T00:00:00Z".into(),
            },
            EventBody::ToolCallUpdated {
                turn_id: "t".into(),
                tool_call_id: "tc1".into(),
                arguments: None,
            },
            EventBody::ToolCallCompleted {
                turn_id: "t".into(),
                tool_call_id: "tc1".into(),
                result: None,
                public_error: None,
            },
            EventBody::PermissionRequested {
                permission_id: "p1".into(),
                turn_id: "t".into(),
                tool_call_id: None,
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
}
