//! Action envelope 与方法面（§4.3 / §4.3.1）。
//!
//! `ActionEnvelope` 是第二层 internally tagged 枚举（tag `"type"`），与
//! [`Frame::Action`](crate::Frame::Action) 的外层 `"t"` 嵌套序列化为文档 §4.3
//! 形态：`{"t":"action","commandId":…,"type":"session/prompt","payload":{…}}`。
//!
//! `type` 判别放在 envelope 层而非 payload untagged 枚举：`session/load` 与
//! `session/close` 的 payload 同为 `{ session_id }` 单字段，untagged 无法区分。

use serde::{Deserialize, Serialize};

/// Action 方法面（§4.3 表 + §4.3.1）。
///
/// 全部携带 `command_id`（uuid 形态，幂等键，同 session 唯一；重试复用同一
/// ID 绝不可换 ID 猜测结果）。文档「uuid」不做格式强校验，幂等键语义在 server。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionEnvelope {
    /// 创建会话；`machine_id` 缺省 = 本机（§4.3）。
    #[serde(rename = "session/create", rename_all = "camelCase")]
    Create {
        command_id: String,
        payload: CreateSessionPayload,
    },
    /// 载入既有会话（M2，类型保留；转发前开启回放窗口）。
    #[serde(rename = "session/load", rename_all = "camelCase")]
    Load {
        command_id: String,
        payload: LoadSessionPayload,
    },
    /// 关闭并 kill 对应 ACP 进程（offline 时语义见架构 §7.6）。
    #[serde(rename = "session/close", rename_all = "camelCase")]
    Close {
        command_id: String,
        payload: CloseSessionPayload,
    },
    /// 转发 prompt 到目标 machine。
    #[serde(rename = "session/prompt", rename_all = "camelCase")]
    Prompt {
        command_id: String,
        payload: PromptSessionPayload,
    },
    /// 转发 cancel（携带目标 session_id，路由据此精确投递）。
    #[serde(rename = "session/cancel", rename_all = "camelCase")]
    Cancel {
        command_id: String,
        payload: CancelSessionPayload,
    },
    /// 权限应答（CAS 校验通过后才下发，见架构 §7.4）。
    #[serde(rename = "permission/resolve", rename_all = "camelCase")]
    ResolvePermission {
        command_id: String,
        payload: ResolvePermissionPayload,
    },
    /// 原始 ACP 事件订阅（M3，类型保留；`from_seq` 缺省 = 实时起）。
    #[serde(rename = "events/subscribe", rename_all = "camelCase")]
    SubscribeEvents {
        command_id: String,
        payload: SubscribeEventsPayload,
    },
    /// 事件退订（M3，类型保留）。
    #[serde(rename = "events/unsubscribe", rename_all = "camelCase")]
    UnsubscribeEvents {
        command_id: String,
        payload: UnsubscribeEventsPayload,
    },
}

impl ActionEnvelope {
    /// serde `type` 判别值（§4.3 方法面；与 `whitelist::M1_ACTION_TYPES`
    /// 对照用于 §4.8 action type 收窄检查）。
    pub fn type_str(&self) -> &'static str {
        match self {
            ActionEnvelope::Create { .. } => "session/create",
            ActionEnvelope::Load { .. } => "session/load",
            ActionEnvelope::Close { .. } => "session/close",
            ActionEnvelope::Prompt { .. } => "session/prompt",
            ActionEnvelope::Cancel { .. } => "session/cancel",
            ActionEnvelope::ResolvePermission { .. } => "permission/resolve",
            ActionEnvelope::SubscribeEvents { .. } => "events/subscribe",
            ActionEnvelope::UnsubscribeEvents { .. } => "events/unsubscribe",
        }
    }
}

/// `session/create` payload；`machine_id`/`cwd`/`title` 均可缺省，服务端按
/// 连接绑定补充与校验（§4.3），客户端字段不可覆盖 binding。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionPayload {
    /// 目标 machine；缺省 = 本机（P5）。
    pub machine_id: Option<String>,
    /// 工作目录；未指定时 server 注入已认证上下文默认目录（§4.3 裁决）。
    pub cwd: Option<String>,
    /// 会话标题。
    pub title: Option<String>,
}

/// `session/load` payload（M2，类型保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionPayload {
    pub session_id: String,
}

/// `session/close` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSessionPayload {
    pub session_id: String,
}

/// `session/prompt` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSessionPayload {
    pub session_id: String,
    pub message: String,
}

/// `session/cancel` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSessionPayload {
    pub session_id: String,
}

/// `permission/resolve` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvePermissionPayload {
    pub session_id: String,
    pub permission_id: String,
    pub decision: PermissionDecision,
}

/// `events/subscribe` payload（M3，类型保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeEventsPayload {
    /// 缺省 = 订阅全部可见 session。
    pub session_id: Option<String>,
    /// 缺省 = 实时起（不重放历史）；带 `from_seq` 则从该序号起推。
    pub from_seq: Option<u64>,
}

/// `events/unsubscribe` payload（M3，类型保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeEventsPayload {
    /// 缺省 = 退订全部。
    pub session_id: Option<String>,
}

/// 权限决议（§4.3）；也供 §7 schema 的 `PermissionProjection.decision` 复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
}
