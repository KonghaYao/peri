//! Registry Doc 类型镜像（§5.5，acp-hub 特有）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{GlobalStatus, MachineStatus};

/// Registry Doc 根对象（§5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryDocRoot {
    /// == [`crate::version::REGISTRY_DOC_SCHEMA_VERSION`]。
    pub schema_version: u32,
    pub machines: HashMap<String, MachineView>,
    /// 活跃会话摘要——唯一权威源，server 状态源单写（§5.2 裁决）。
    pub sessions: HashMap<String, SessionSummary>,
    pub global: RegistryGlobal,
}

/// 机器视图（§5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineView {
    pub id: String,
    pub hostname: String,
    pub status: MachineStatus,
    /// 只暴露 token_id，绝不暴露 token 本体（§9.2.1）。
    pub token_id: String,
    /// RFC3339。
    pub registered_at: String,
    /// RFC3339。
    pub last_heartbeat: String,
    pub session_count: u32,
}

/// 活跃会话摘要（§5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub machine_id: String,
    pub title: String,
    /// 【决策】§5.5 未展开，M1 以架构 §7.3 session 状态字符串透传。
    pub status: String,
    /// 补推缺口（§8.5），无缺口为 null。
    pub gap: Option<u64>,
    /// RFC3339。
    pub updated_at: String,
}

/// 全局状态（§5.5 `global: { status }`；Degraded 判定规则见架构 §17.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryGlobal {
    pub status: GlobalStatus,
}
