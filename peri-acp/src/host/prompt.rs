//! ACP Prompt execution — builds and executes the agent via crate::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::sync::Arc;

use crate::broker::AcpTransportBroker;
use crate::session::event_sink::TransportEventSink;
use crate::session::executor;
use crate::transport::types::AcpError;
use agent_client_protocol::schema::v1::{Meta, PromptResponse, StopReason};
use peri_acp_types::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse,
    TrustedInteractionOwner, UserInteractionBroker,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::session::{ExecutionFailure, ExecutionFailureKind};
use peri_acp_types::thread::{ThreadId, WorkState};
use serde_json::Value;
use tracing::info;

use peri_agent::session::exec::executor_helpers::CommandLookupFn;

mod model_factories;
use model_factories as models;
mod prompt_outcome;
use prompt_outcome as outcome;
mod stage;
mod telemetry;
pub(super) use outcome::run_prompt_with_outcome;
pub(crate) use stage::build_compact_hooks;

use super::SharedSessions;

pub use prompt_turn::ACP_TURN_EXECUTION_FAILED_CODE;

#[cfg(test)]
pub(crate) use prompt_turn::execution_failure_to_acp_error;

pub(crate) use prompt_turn::build_transport_broker;

pub(crate) use prompt_turn::approve_scheduled_trigger;

#[cfg(test)]
pub(crate) use prompt_turn::run_prompt;

#[cfg(test)]
pub(super) use prompt_turn::ensure_resident_context;

#[cfg(test)]
pub(super) use prompt_turn::finish_prompt_turn;

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;

#[path = "prompt/prompt_turn.rs"]
mod prompt_turn;
