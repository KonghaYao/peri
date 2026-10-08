//! Local project identity and immutable session execution binding.

use crate::thread::{ThreadId, ThreadListEntry};
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
opaque_id!(MachineId);

/// 机器记录的来源。迁移占位记录不能被当作本机执行身份使用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineIdentityKind {
    Known,
    LegacyUnknown,
}

/// Machine 的展示投影；名称不参与身份比较。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub id: String,
    pub name: String,
    pub identity_kind: MachineIdentityKind,
    pub is_current: bool,
}

/// Workspace 路径的发现依据，决定其能否宣称为已验证的 Git 根。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePathSource {
    Discovered,
    DerivedLegacy,
    Unverified,
}

/// 会话归属的 Workspace 投影；执行目录仍由各 Session 自己保存。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub machine_id: String,
    pub path: PathBuf,
    pub path_source: WorkspacePathSource,
}

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

impl SessionBinding {
    /// 以 workspace 事实构造不可变绑定。
    ///
    /// 版本与 revision 由契约固定，调用方只能提供已解析的 workspace——创建入口
    /// （门面 `create_session`）不接受调用方自行拼装的绑定字段。
    pub fn from_workspace(workspace: &ResolvedWorkspace) -> Self {
        Self {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: workspace.project_id,
            workspace_id: workspace.workspace_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        }
    }
}

/// A validated execution directory. The store revalidates this before binding a thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWorkspace {
    pub project_id: ProjectId,
    /// Machine/path ownership identity: the session binding and the session row both
    /// name this one Workspace, and its recorded evidence is what rechecks execution.
    pub workspace_id: WorkspaceId,
    pub cwd: PathBuf,
    pub root: PathBuf,
    pub relative_cwd: PathBuf,
    /// Internal creation evidence. Never expose filesystem identity bytes on ACP wire.
    #[serde(skip)]
    pub discovery_snapshot: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ThreadScope {
    Environment(String),
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

/// 本机 workspace/binding 语义的失败分类。
///
/// `Clone`：错误在落到 `SessionResourceError` 之前会经过 `anyhow` 链，资源层需要按
/// 原分类重建同一个错误值（不重新解释、不丢变体）。
#[derive(Clone, Debug, thiserror::Error)]
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
    /// 会话存储以只读方式打开：历史可读，登记新工作区与新会话不可用。
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

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
