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
            workspace_id: workspace.execution_registration_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        }
    }
}

/// A validated execution directory. The store revalidates this before binding a thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWorkspace {
    pub project_id: ProjectId,
    /// Stable Machine/path ownership identity for Session grouping.
    pub workspace_id: WorkspaceId,
    /// Local filesystem discovery registration used only to recheck execution evidence.
    pub execution_registration_id: WorkspaceId,
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

impl WorkspaceErrorData {
    /// 按 workspace 失败构造类型化数据；本集合之外的失败不带数据（调用方只看消息）。
    ///
    /// 这里是「哪些失败可以被客户端分派到具体修复动作」的唯一清单：加一条就在
    /// [`WorkspaceError`] 上多一个可编程分支，因此只收需要客户端采取不同动作的变体。
    pub fn from_workspace_error(error: &WorkspaceError) -> Option<Self> {
        match error {
            WorkspaceError::RecoveryRequired(details) => {
                Some(Self::RecoveryRequired(details.clone()))
            }
            _ => None,
        }
    }
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
    /// The former execution owner cannot be proven stopped; history remains readable.
    #[serde(rename = "peri.formerOwnerUnverifiedV1")]
    FormerOwnerUnverified,
}

/// A restored session may be executable while prior local work remains unverified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionRestoreWarning {
    FormerOwnerUnverified,
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

/// 本机 workspace/binding/owner 语义的失败分类。
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

/// Local runtime lifecycle handle, retained until its resources have stopped.
/// This handle does not provide exclusive session ownership or an OS lock.
#[async_trait]
pub trait SessionExecutionLease: Send + Sync {
    fn thread_id(&self) -> &ThreadId;
    /// Store-issued execution generation. A missing token has no cross-process write authority.
    fn owner_token(&self) -> Option<ExecutionOwnerToken> {
        None
    }
    fn prior_unreleased_generation(&self) -> Option<PriorExecutionOwner> {
        None
    }
    /// Drain admitted writes and close this runtime handle only after its resources have stopped.
    /// An unknown persistence outcome prevents successful completion; no execution state is persisted.
    async fn mark_clean(&self) -> anyhow::Result<()>;
}

/// A Store-issued root execution claim. Both values are required for a write fence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionOwnerToken {
    pub root_id: ThreadId,
    pub epoch: i64,
    pub nonce: String,
}

/// Previous owner evidence returned by the Store CAS. A missing generation ID
/// still requires proof and must fail closed at executable admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriorExecutionOwner {
    pub agent_generation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionOwnerClaim {
    pub token: ExecutionOwnerToken,
    pub prior_unreleased: Option<PriorExecutionOwner>,
}

/// Nonsecret identity of the only external async owner that supports takeover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceExecutionDescriptor {
    pub endpoint: String,
    pub owner_identity: String,
    pub agent_generation_id: String,
    pub unsupported_async_owners: bool,
}

impl WorkspaceExecutionDescriptor {
    /// The recoverable case must carry a complete, nonsecret authority identity.
    pub fn valid_for_store(&self) -> bool {
        self.endpoint.len() <= 4096
            && self.owner_identity.len() <= 256
            && self.agent_generation_id.len() <= 256
            && (self.unsupported_async_owners
                || (!self.endpoint.is_empty()
                    && self.owner_identity.len() == 64
                    && self
                        .owner_identity
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    && !self.agent_generation_id.is_empty()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionWorkspaceOwnerRecord {
    pub current_epoch: i64,
    pub descriptor_epoch: i64,
    pub descriptor: WorkspaceExecutionDescriptor,
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
