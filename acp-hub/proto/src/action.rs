//! Action envelope 与方法面（§4.3 / §4.3.1）。
//!
//! `ActionEnvelope` 是第二层 internally tagged 枚举（tag `"type"`），与
//! [`Frame::Action`](crate::Frame::Action) 的外层 `"t"` 嵌套序列化为文档 §4.3
//! 形态：`{"t":"action","commandId":…,"type":"chat/prompt","payload":{…}}`。
//!
//! `type` 判别放在 envelope 层而非 payload untagged 枚举：`chat/load` 与
//! `chat/close` 的 payload 同为 `{ chat_id }` 单字段，untagged 无法区分。

use serde::{Deserialize, Serialize};

/// Action 方法面（§4.3 表 + §4.3.1）。
///
/// 全部携带 `command_id`（uuid 形态，幂等键，同 chat 唯一；重试复用同一
/// ID 绝不可换 ID 猜测结果）。文档「uuid」不做格式强校验，幂等键语义在 server。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionEnvelope {
    /// 创建对话；`instance_id` 缺省 = 本机（§4.3）。
    #[serde(rename = "chat/create", rename_all = "camelCase")]
    Create {
        command_id: String,
        payload: CreateChatPayload,
    },
    /// 载入既有会话（M2，类型保留；转发前开启回放窗口）。
    #[serde(rename = "chat/load", rename_all = "camelCase")]
    Load {
        command_id: String,
        payload: LoadChatPayload,
    },
    /// 关闭并 kill 对应 ACP 进程（offline 时语义见架构 §7.6）。
    #[serde(rename = "chat/close", rename_all = "camelCase")]
    Close {
        command_id: String,
        payload: CloseChatPayload,
    },
    /// 转发 prompt 到目标 instance。
    #[serde(rename = "chat/prompt", rename_all = "camelCase")]
    Prompt {
        command_id: String,
        payload: PromptChatPayload,
    },
    /// 转发 cancel（携带目标 chat_id，路由据此精确投递）。
    #[serde(rename = "chat/cancel", rename_all = "camelCase")]
    Cancel {
        command_id: String,
        payload: CancelChatPayload,
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
    /// 创建工作区（独立于 chat 的上层概念：定义本地目录 cwd，其下新建的
    /// 对话继承该 cwd——ACP 进程工作目录与 session/list 查询面一致）。
    #[serde(rename = "workspace/create", rename_all = "camelCase")]
    WorkspaceCreate {
        command_id: String,
        payload: WorkspaceCreatePayload,
    },
    /// 删除工作区定义（不影响已建对话与会话；仅移除目录定义）。
    #[serde(rename = "workspace/remove", rename_all = "camelCase")]
    WorkspaceRemove {
        command_id: String,
        payload: WorkspaceRemovePayload,
    },
    /// 查询指定对话的 ACP 会话列表（§6.3 按需查询）：server 从 chat record
    /// 解析 (instance_id, cwd) 后向 agent 侧发 `session/list` RPC，结果经
    /// `session_list` 下行帧回投。agent 侧是真实数据源——不依赖轮询投影。
    #[serde(rename = "session/list", rename_all = "camelCase")]
    SessionList {
        command_id: String,
        payload: SessionListPayload,
    },
}

impl ActionEnvelope {
    /// serde `type` 判别值（§4.3 方法面；与 `whitelist::M1_ACTION_TYPES`
    /// 对照用于 §4.8 action type 收窄检查）。
    pub fn type_str(&self) -> &'static str {
        match self {
            ActionEnvelope::Create { .. } => "chat/create",
            ActionEnvelope::Load { .. } => "chat/load",
            ActionEnvelope::Close { .. } => "chat/close",
            ActionEnvelope::Prompt { .. } => "chat/prompt",
            ActionEnvelope::Cancel { .. } => "chat/cancel",
            ActionEnvelope::ResolvePermission { .. } => "permission/resolve",
            ActionEnvelope::SubscribeEvents { .. } => "events/subscribe",
            ActionEnvelope::UnsubscribeEvents { .. } => "events/unsubscribe",
            ActionEnvelope::WorkspaceCreate { .. } => "workspace/create",
            ActionEnvelope::WorkspaceRemove { .. } => "workspace/remove",
            ActionEnvelope::SessionList { .. } => "session/list",
        }
    }
}

/// `chat/create` payload；`instance_id`/`cwd`/`title` 均可缺省，服务端按
/// 连接绑定补充与校验（§4.3），客户端字段不可覆盖 binding。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatPayload {
    /// 目标 instance；缺省 = 本机（P5）。
    pub instance_id: Option<String>,
    /// 工作目录；未指定时 server 注入已认证上下文默认目录（§4.3 裁决）。
    pub cwd: Option<String>,
    /// 会话标题。
    pub title: Option<String>,
    /// ACP 历史会话恢复（§8.5）：携带 session/list 返回的 acp_session_id 时，
    /// create 序列走 `session/load`（回放历史）而非 `session/new`。
    pub acp_session_id: Option<String>,
    /// 工作区归属（workspace/create 返回后可用）：存在时对话继承该工作区的
    /// cwd（优先级高于 `cwd`）；不存在时 `cwd` 生效；两者皆缺省 → server 默认目录。
    pub workspace_id: Option<String>,
}

/// `chat/load` payload：在当前对话（其 ACP 进程）内切换会话（§8.5）。
///
/// 点击 SessionList 历史会话 → 前端向**当前对话**发 load——进程不新建，
/// 直接把目标历史会话加载为进程的当前会话（会话是进程内实体，随进程
/// 消亡；一个进程可先后持有多个会话）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadChatPayload {
    pub chat_id: String,
    /// 目标 ACP 会话 id（来自 session/list 响应；须属于该 chat 的进程）。
    pub acp_session_id: String,
}

/// `chat/close` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseChatPayload {
    pub chat_id: String,
}

/// `chat/prompt` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptChatPayload {
    pub chat_id: String,
    pub message: String,
}

/// `chat/cancel` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelChatPayload {
    pub chat_id: String,
}

/// `permission/resolve` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvePermissionPayload {
    pub chat_id: String,
    pub permission_id: String,
    pub decision: PermissionDecision,
}

/// `events/subscribe` payload（M3，类型保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeEventsPayload {
    /// 缺省 = 订阅全部可见 chat。
    pub chat_id: Option<String>,
    /// 缺省 = 实时起（不重放历史）；带 `from_seq` 则从该序号起推。
    pub from_seq: Option<u64>,
}

/// `events/unsubscribe` payload（M3，类型保留）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeEventsPayload {
    /// 缺省 = 退订全部。
    pub chat_id: Option<String>,
}

/// 权限决议（§4.3）；也供 §7 schema 的 `PermissionProjection.decision` 复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// `workspace/create` payload：定义本地目录（cwd），其下新建对话继承。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCreatePayload {
    /// 工作区名称（展示用；可为空串 → server 以目录名兜底）。
    pub name: String,
    /// 本地绝对目录（server 校验存在性；spawn/initialize/session/list 均
    /// 以它为工作目录）。
    pub cwd: String,
}

/// `workspace/remove` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRemovePayload {
    pub workspace_id: String,
}

/// `session/list` payload：按需查询指定对话的 ACP 会话列表（§6.3）。
///
/// cwd/instance 不信任客户端直传——server 从 chat record 解析
/// （spawn/initialize/session/list 的查询面一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListPayload {
    pub chat_id: String,
}
