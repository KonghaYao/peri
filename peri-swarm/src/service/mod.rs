//! Service port consumed by the HTTP layer.
//!
//! S3 ships [`InMemorySwarmService`] as a deterministic fake. S4 replaces it
//! with an ACP-backed actor service that implements the same trait; the HTTP
//! layer never branches on which implementation is present.

mod in_memory;

pub use in_memory::{InMemorySwarmService, ServiceCall, ServiceConfig, TurnPolicy};

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::domain::{
    Cursor, EnvironmentKind, InputOutcome, InputSubmission, ItemPage, ItemsWindow, LogicalId,
    SessionView, StreamEvent, SwarmError,
};

/// Fully validated create command. The HTTP layer owns request parsing and cwd
/// normalization; the service owns identity and turn semantics.
#[derive(Clone, Debug)]
pub struct CreateSessionCommand {
    pub logical_id: LogicalId,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub instructions: Option<String>,
    pub initial_input: Option<String>,
    pub environment: EnvironmentKind,
    /// Root ACP session id reserved by the caller (S5 gateway). A trusted host
    /// generates it before routing to the actor so a retried create addresses
    /// the same identity; `None` keeps the S4 behavior (service generates one).
    pub root_session_id: Option<String>,
}

/// Create response payload shared by the in-memory fake and the actor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResult {
    pub session: SessionView,
    pub created: bool,
}

/// A live SSE subscription: persisted backlog first, then broadcast events.
///
/// Implementations must create the broadcast receiver **before** reading the
/// backlog so no event can fall between replay and live delivery.
#[derive(Debug)]
pub struct EventSubscription {
    backlog: Vec<StreamEvent>,
    live: broadcast::Receiver<StreamEvent>,
}

impl EventSubscription {
    #[must_use]
    pub fn new(backlog: Vec<StreamEvent>, live: broadcast::Receiver<StreamEvent>) -> Self {
        Self { backlog, live }
    }

    #[must_use]
    pub fn into_parts(self) -> (Vec<StreamEvent>, broadcast::Receiver<StreamEvent>) {
        (self.backlog, self.live)
    }

    #[must_use]
    pub fn backlog(&self) -> &[StreamEvent] {
        &self.backlog
    }
}

/// Typed port between HTTP and the session runtime (S4 actor / S3 fake).
#[async_trait]
pub trait SwarmService: Send + Sync {
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError>;

    async fn get_session(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError>;

    async fn submit_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<InputOutcome, SwarmError>;

    async fn list_items(
        &self,
        logical_id: &LogicalId,
        window: ItemsWindow,
    ) -> Result<ItemPage, SwarmError>;

    /// Subscribe to an existing session; unknown sessions are `404 unknown_agent`.
    async fn subscribe(
        &self,
        logical_id: &LogicalId,
        cursor: Cursor,
    ) -> Result<EventSubscription, SwarmError>;

    /// Register a stream before `POST /sessions` executes the first turn.
    /// Must not create or touch session rows.
    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription;
}
