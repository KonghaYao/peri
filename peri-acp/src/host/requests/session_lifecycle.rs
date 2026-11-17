//! Session 生命周期命令 handler：initialize / new / load / list /
//! close / delete / resume / fork / rename（自 requests.rs 拆出，请求分发见
//! `host/requests.rs`）。后台任务查询/取消在 [`session_tasks`]。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CloseSessionResponse, DeleteSessionResponse, ForkSessionResponse, ListSessionsResponse,
    NewSessionResponse, SessionId,
};
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, FrozenState, NewSessionDraft, NewSessionMeta, SessionInitialization,
    SessionMetaPatch,
};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{info, warn};

use super::super::notify;
use super::super::AcpServerConfig;
use super::super::SessionState;
use crate::dispatch;
use crate::dispatch::config_update;
use crate::transport::types::AcpError;

#[path = "session_restore.rs"]
pub(crate) mod restore;
#[path = "session_apps_capability.rs"]
mod session_apps_capability;
pub(crate) use restore::{
    assemble_for_recovery, handle_context, handle_fork_shared, handle_load, handle_load_shared,
    handle_metadata, handle_resume, handle_resume_shared, rollback_unused_restore_publication,
    RestorePublication,
};
#[cfg(test)]
pub(crate) use restore::{
    cleanup_unpublished_restore, publication_test_hook, rollback_restore_publication,
};

#[cfg(not(target_os = "emscripten"))]
pub(super) use session_lifecycle_dispatch::create_session_workflow_chain;

#[cfg(not(target_os = "emscripten"))]
pub(super) use session_lifecycle_dispatch::workflow_face_closed;

#[cfg(not(target_os = "emscripten"))]
pub(super) use session_lifecycle_dispatch::attach_session_workflow;

pub(crate) use session_lifecycle_dispatch::handle_initialize;

pub(crate) use session_lifecycle_dispatch::initialize_client_meta;
pub(crate) use session_lifecycle_dispatch::initialize_peri_caps;

pub(super) use session_lifecycle_dispatch::enabled_meta_sections;

#[path = "session_mcp_setup.rs"]
mod session_mcp_setup;

pub(crate) use session_lifecycle_dispatch::handle_new;

#[cfg(test)]
pub(crate) use session_lifecycle_dispatch::new_session_from_prepared;

#[cfg(test)]
pub(super) use session_lifecycle_dispatch::drain_and_abandon;

pub(crate) use session_lifecycle_dispatch::after_new_response;

pub(crate) use session_lifecycle_dispatch::handle_list;

#[path = "session_tasks.rs"]
mod session_tasks;
pub(crate) use session_tasks::{handle_bg_tasks, handle_cancel_bg_task};

#[path = "session_close.rs"]
mod close;
pub(super) use close::close_session;

pub(crate) use session_lifecycle_dispatch::handle_close;

pub(crate) use session_lifecycle_dispatch::handle_delete;

pub(crate) use session_lifecycle_dispatch::handle_fork;

pub(super) use session_lifecycle_dispatch::handle_rename;

pub(super) use session_lifecycle_dispatch::prewarm_session_mcp_discovery;

#[path = "session_lifecycle/session_lifecycle_dispatch.rs"]
mod session_lifecycle_dispatch;
