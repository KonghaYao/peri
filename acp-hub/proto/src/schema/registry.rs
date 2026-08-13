//! Registry Doc 类型镜像（§5.5，acp-hub 特有）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{GlobalStatus, InstanceStatus};

/// Registry Doc 根对象（§5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryDocRoot {
    /// == [`crate::version::REGISTRY_DOC_SCHEMA_VERSION`]。
    pub schema_version: u32,
    pub instances: HashMap<String, InstanceView>,
    /// 活跃 chat 摘要——唯一权威源，server 状态源单写（§5.2 裁决）。
    pub chats: HashMap<String, ChatSummary>,
    #[serde(default)]
    pub projects: HashMap<String, ProjectSummary>,
    #[serde(default)]
    pub project_sessions: HashMap<String, ProjectSessionSummary>,
    pub global: RegistryGlobal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub instance_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSessionSummary {
    pub id: String,
    pub project_id: String,
    pub acp_session_id: Option<String>,
    pub title: String,
    pub lifecycle: String,
    pub updated_at: String,
    pub last_opened_at: Option<String>,
    pub active_chat_id: Option<String>,
}

/// 实例视图（§5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceView {
    pub id: String,
    pub hostname: String,
    pub status: InstanceStatus,
    /// 只暴露 token_id，绝不暴露 token 本体（§9.2.1）。
    pub token_id: String,
    /// RFC3339。
    pub registered_at: String,
    /// RFC3339。
    pub last_heartbeat: String,
    pub chat_count: u32,
}

/// 活跃 chat 摘要（§5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub id: String,
    pub instance_id: String,
    pub title: String,
    /// 【决策】§5.5 未展开，M1 以架构 §7.3 chat 状态字符串透传。
    pub status: String,
    /// 补推缺口（§8.5），无缺口为 null。
    pub gap: Option<u64>,
    /// RFC3339。
    pub updated_at: String,
    /// ACP 进程工作目录（继承自 workspace 或 server 默认目录）。
    pub cwd: String,
    /// 归属工作区（无 → null；工作区删除后已建对话保留此引用）。
    pub workspace_id: Option<String>,
}

/// 工作区摘要（独立于 chat 的上层概念：定义本地目录 cwd，其下新建对话
/// 继承；Registry Doc `workspaces` map 值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub id: String,
    pub name: String,
    /// 本地绝对目录（ACP 进程工作目录 / session/list 查询面）。
    pub cwd: String,
    /// RFC3339。
    pub created_at: String,
    /// RFC3339。
    pub updated_at: String,
}

/// 全局状态（§5.5 `global: { status }`；Degraded 判定规则见架构 §17.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryGlobal {
    pub status: GlobalStatus,
}
