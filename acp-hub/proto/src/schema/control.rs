//! Control Doc 类型镜像（§5.4）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    PermissionOptions, PermissionStatus, PublicError, ChatStatus, TurnStatus,
};
use crate::action::PermissionDecision;

/// Control Doc 根对象（§5.4）。
///
/// 旧快照恢复时以 `schema_version` 判空幂等补结构（§5.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDocRoot {
    /// == [`crate::version::SESSION_DOC_SCHEMA_VERSION`]。
    pub schema_version: u32,
    pub projection_version: u32,
    pub chat: ChatInfoProjection,
    pub agent: AgentStatusProjection,
    /// 权威投影，前端由 turn_status 派生展示（架构 §7.2）。
    pub active_turn: Option<ActiveTurnProjection>,
    pub pending_permissions: HashMap<String, PermissionProjection>,
    /// agent 磁盘历史会话条目（§5.2 裁决：与 Registry 活跃会话语义不同、互不替代）。
    pub sessions: HashMap<String, SessionSummaryProjection>,
}

/// 会话元信息投影（§5.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatInfoProjection {
    pub chat_id: String,
    pub title: String,
    /// 【决策】值域见 [`ChatStatus`]（架构 §7.3）。
    pub status: ChatStatus,
    pub active_turn_id: Option<String>,
    /// RFC3339。
    pub created_at: String,
    /// RFC3339。
    pub updated_at: String,
}

/// Agent 状态投影（§5.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusProjection {
    pub instance_id: String,
    pub session_id: String,
    /// 【决策】agent 状态值域文档未展开，M1 透传 ACP agent 状态。
    pub status: String,
    pub capabilities: Vec<String>,
    /// RFC3339。
    pub last_activity_at: String,
    pub public_error: Option<PublicError>,
}

/// 活动 turn 投影（§5.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTurnProjection {
    pub turn_id: String,
    pub turn_status: TurnStatus,
    /// RFC3339。
    pub updated_at: String,
}

/// 权限请求投影（§5.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProjection {
    pub permission_id: String,
    pub turn_id: String,
    pub tool_call_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub options: Vec<PermissionOptions>,
    pub status: PermissionStatus,
    /// server 权威时钟生成（架构 §4.7）；RFC3339。
    pub expires_at: String,
    /// CAS 迁移成功后写入；expired 保持 null。
    pub decision: Option<PermissionDecision>,
}

/// agent 磁盘历史会话条目（§5.4）。
///
/// 【决策】§5.4 未展开字段；M1 以最小摘要实现，与架构 §15 映射对齐时定稿。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummaryProjection {
    pub session_id: String,
    pub title: String,
    pub status: String,
    /// RFC3339。
    pub updated_at: String,
}
