//! A session owns one verified execution environment and its resource lifetime.

#[path = "workspace_workflow.rs"]
mod workflow;

use std::sync::Arc;

use super::{assemble, task_scope, AcpServerConfig};
use crate::transport::types::AcpError;
use peri_acp_types::ports::McpBuiltinWorkspaceState;
use peri_acp_types::session_resources::{BindingRecheck, SessionResourceError};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::ResolvedWorkspace;

pub(crate) use session_environment::SessionEnvironment;

#[cfg(test)]
pub(crate) use session_environment::take_shutdown_observations;

pub(crate) use session_environment::frozen_runtime_env;

pub(crate) use session_environment::workspace_error;

pub(crate) use session_environment::resource_error;

pub(crate) use session_environment::BindingCheck;

pub(crate) use session_environment::validate_expected;

pub(crate) use session_environment::reassert_expected;

#[path = "workspace/session_environment.rs"]
mod session_environment;
