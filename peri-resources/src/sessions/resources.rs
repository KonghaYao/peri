//! 会话数据资源门面：工作区校验、读写权限与持久化结清。
//! 执行所有权由 peri-sdk 维护，本模块不登记或认领执行者。

mod admission;
mod assembly;
mod child_resume_claim_handle;
mod session_resources_impl;
use child_resume_claim_handle as claim;
mod evidence;
mod mutation_gate;
use mutation_gate as gate;
mod lifecycle;
mod lifecycle_credentials;
use lifecycle_credentials as oauth_credentials;

use session_resources_impl::Path;

#[cfg(not(target_os = "emscripten"))]
use session_resources_impl::PathBuf;

use session_resources_impl::Arc;

use session_resources_impl::{
    BindingRecheck, BindingState, ExecutionAvailability, FrozenState, NewSession,
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
#[cfg(test)]
use session_resources_impl::{ChildSnapshot, FrozenSnapshotBytes};

#[cfg(test)]
use session_resources_impl::PersistedPayload;

use session_resources_impl::ThreadId;

use session_resources_impl::{ResolvedWorkspace, SessionBinding, WorkspaceError};

use session_resources_impl::SessionDataPort;

use session_resources_impl::{SwarmStorePort, UnsupportedSwarmStore};

#[cfg(target_os = "emscripten")]
use session_resources_impl::execution_failure;

use session_resources_impl::{invalid_input, not_found};

use session_resources_impl::LocalExecutionPort;

#[cfg(not(target_os = "emscripten"))]
use session_resources_impl::execution_failure;

#[cfg(not(target_os = "emscripten"))]
use session_resources_impl::{LocalExecution, ReadOnlyThreadStoreError};

use session_resources_impl::MutationGate;

use session_resources_impl::{Lifecycle, LifecycleState};

use session_resources_impl::SETTLE_WAIT;

pub(in crate::sessions) use session_resources_impl::SessionDataHome;

pub use session_resources_impl::SessionResourcesImpl;

#[path = "resources/deployment.rs"]
mod deployment;

#[cfg(test)]
#[path = "resources_test.rs"]
mod tests;
