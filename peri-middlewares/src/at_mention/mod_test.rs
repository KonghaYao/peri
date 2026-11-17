//! @path reads through the session Workspace reader, never through the host cwd.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_agent::agent::stages::middleware_runner;
use peri_agent::agent::stages::receive as receive_functions;
use peri_agent::agent::stages::ReceiveInput;
use peri_agent::agent::stages::StageContext;
use peri_agent::agent::state::AgentState;
use peri_agent::middleware::MiddlewareChain;
use peri_agent::session::FrozenContext;
use peri_agent::session::MessageSource;
use peri_agent::session::QueuedMessage;
use peri_agent::session::Session;
use tempfile;

use super::async_trait;
use super::AtMentionMiddleware;
use super::BaseMessage;
use super::WorkspaceFileReader;
use super::MAX_BATCH_MENTION_BYTES;
use super::MAX_BATCH_MENTION_ITEMS;
use super::MAX_MENTION_CONTENT_BYTES;
use crate::workspace_io::{WorkspaceMentionContent, WorkspaceReadError};

#[path = "work_fixture.rs"]
pub(crate) mod work_fixture;

#[path = "mod_test/test_cases.rs"]
mod test_cases;
