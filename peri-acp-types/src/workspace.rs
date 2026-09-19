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
    #[error("workspace identity changed; explicit relinking is required")]
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
    #[error("session database schema or version is unsupported")]
    UnsupportedDatabaseSchema,
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
