//! Local project identity, immutable session execution binding and ownership ports.

use crate::thread::{ThreadId, ThreadListEntry};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, str::FromStr};
use uuid::Uuid;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);
        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}
opaque_id!(ProjectId);
opaque_id!(WorkspaceId);

pub const SESSION_BINDING_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBinding {
    pub schema_version: u16,
    /// Protocol compatibility field, always 1 for immutable bindings; not persisted.
    pub revision: u64,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub cwd_relative_to_workspace: PathBuf,
}

/// A validated execution directory. The store revalidates this before binding a thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWorkspace {
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub cwd: PathBuf,
    pub root: PathBuf,
    pub relative_cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ThreadScope {
    Project(ProjectId),
    Workspace(WorkspaceId),
    ExactDirectory {
        workspace_id: WorkspaceId,
        relative_cwd: PathBuf,
    },
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadListCursor {
    pub updated_at: DateTime<Utc>,
    pub thread_id: ThreadId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopedThreadQuery {
    pub scope: ThreadScope,
    pub cursor: Option<ThreadListCursor>,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopedThreadEntry {
    pub thread: ThreadListEntry,
    /// None for legacy history. Displaying a saved path does not establish execution identity.
    pub binding: Option<SessionBinding>,
    /// Last registered location, for display only; executable load must validate it.
    pub effective_cwd: PathBuf,
    pub workspace_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopedThreadPage {
    pub entries: Vec<ScopedThreadEntry>,
    pub next_cursor: Option<ThreadListCursor>,
}

/// 精确标识待解除的 dirty 代际；不是旧执行已结束的证明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequiredDetails {
    pub thread_id: ThreadId,
    pub generation: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details")]
pub enum WorkspaceErrorData {
    #[serde(rename = "peri.recoveryRequiredV1")]
    RecoveryRequired(RecoveryRequiredDetails),
}

/// 只读准入的原因：会话历史可读，但本次准入没有取得执行所有权。
///
/// 客户端据此区分「等待他处释放」与「需要用户显式接受风险解除 dirty」：后者必须
/// 携带精确代际，才能走与 load 失败时相同的确认流程重新取得执行权。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details")]
pub enum ReadOnlyAdmission {
    /// 执行所有权由其他执行宿主持有。
    #[serde(rename = "peri.executionBusyV1")]
    ExecutionBusy,
    /// 上次执行未干净收尾：需要用户显式接受风险解除该代际。
    #[serde(rename = "peri.recoveryRequiredV1")]
    RecoveryRequired(RecoveryRequiredDetails),
    /// 当前节点不提供执行所有权（例如会话存储只读）。
    #[serde(rename = "peri.executionLeaseRequiredV1")]
    ExecutionLeaseRequired,
}

impl ReadOnlyAdmission {
    /// 按存储层给出的不可用原因构造；不在本集合内的原因不降级（调用方原样上报）。
    pub fn from_workspace_error(error: &WorkspaceError) -> Option<Self> {
        match error {
            WorkspaceError::ExecutionBusy => Some(Self::ExecutionBusy),
            WorkspaceError::RecoveryRequired(details) => {
                Some(Self::RecoveryRequired(details.clone()))
            }
            WorkspaceError::ExecutionLeaseRequired => Some(Self::ExecutionLeaseRequired),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetDirtyRequest {
    pub target: RecoveryRequiredDetails,
    pub accept_risk: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace discovery failed: {0}")]
    DiscoveryError(String),
    #[error("workspace location is unavailable")]
    Unavailable,
    #[error(
        "session directory changed; this session cannot continue here: start a new session in the current directory"
    )]
    NeedsRelink,
    #[error("session execution binding does not match the requested environment")]
    ExecutionBindingMismatch,
    #[error("session has no execution binding")]
    BindingMissing,
    #[error("session binding version or data is unsupported")]
    InvalidBinding,
    #[error("session is owned by another execution host")]
    ExecutionBusy,
    #[error("previous session execution did not close cleanly; recovery is required")]
    RecoveryRequired(RecoveryRequiredDetails),
    #[error("dirty generation changed; load again before confirming recovery")]
    RecoveryGenerationMismatch,
    #[error("session mutation requires a live execution lease")]
    ExecutionLeaseRequired,
    /// 会话存储以只读方式打开：历史可读，登记新工作区与新会话不可用。
    ///
    /// 与 `ExecutionLeaseRequired` 的区别在降级空间：那个是「这条会话的执行所有权不在
    /// 本节点」，历史仍可按只读会话进入；这个连「会话」都还没有，没有可降级的对象。
    #[error(
        "session store is read-only; history is readable, but sessions cannot be created or registered here"
    )]
    ReadOnlyStore,
    #[error("session database schema or version is unsupported")]
    UnsupportedDatabaseSchema,
    /// 数据库记录的 `user_version` 本构建不认识。带上实际值与本构建上限，
    /// 让「不支持」这个结论可以追溯到具体版本，而不是停在无原因的描述上。
    #[error(
        "session database schema version {found} is not supported by this build (newest supported: {supported}); use the Peri version that wrote it, or upgrade Peri"
    )]
    UnsupportedSchemaVersion { found: i64, supported: i64 },
    #[error("workspace execution is unsupported by this store")]
    Unsupported,
}

/// Exclusive local execution capability, held across all owned resources.
/// Dropping it releases only the OS lock and deliberately leaves the run dirty.
#[async_trait]
pub trait SessionExecutionLease: Send + Sync {
    fn thread_id(&self) -> &ThreadId;
    /// Persist clean and release ownership only after all owned resources have stopped.
    async fn mark_clean(&self) -> anyhow::Result<()>;
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
