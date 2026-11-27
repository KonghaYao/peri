use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    LoadSessionResponse, ResumeSessionResponse, SessionNotification,
};
use peri_acp_types::session_resources::{BindingRecheck, BindingState, FrozenState};
use peri_acp_types::thread::ThreadId;
use serde_json::Value;

use crate::dispatch;
use crate::dispatch::config_update;
use crate::dispatch::{LoadGeneration, ReplaySender};
use crate::host;
use crate::host::connection::ConnectionFacts;
use crate::host::notify;
use crate::host::prepared::PreparedSessionInputs;
use crate::host::workspace;
use crate::host::workspace::BindingCheck;
use crate::host::AcpServerConfig;
use crate::host::SessionState;
use crate::session::frozen_snapshot;
use crate::transport::types::AcpError;

#[path = "legacy_session.rs"]
mod legacy_session;

#[path = "session_restore/fork_shared.rs"]
mod fork_shared;
#[path = "session_restore/load_shared.rs"]
mod load_shared;
pub(crate) use fork_shared::handle_fork_shared;
#[cfg(test)]
pub(crate) use load_shared::rollback_restore_publication;
#[cfg(test)]
pub(crate) use load_shared::{cleanup_unpublished_restore, publication_test_hook};
pub(crate) use load_shared::{handle_load_shared, handle_resume_shared};
pub(crate) use load_shared::{rollback_unused_restore_publication, RestorePublication};

pub(crate) use session_restore_dispatch::assemble_for_recovery;

pub(crate) use session_restore_dispatch::PreparedSession;

pub(super) use session_restore_dispatch::view_only_error;

pub(crate) use session_restore_dispatch::prepare_existing;

pub(super) use session_restore_dispatch::identity_response;

pub(super) use session_restore_dispatch::response_identity;

pub(crate) use session_restore_dispatch::handle_context;

pub(crate) use session_restore_dispatch::handle_metadata;

pub(crate) use session_restore_dispatch::handle_load;

pub(crate) use session_restore_dispatch::handle_resume;

use session_restore_dispatch::TuiReplaySender;

#[cfg(test)]
#[path = "session_lifecycle_replay_test.rs"]
mod replay_tests;

#[path = "session_restore/session_restore_dispatch.rs"]
mod session_restore_dispatch;
