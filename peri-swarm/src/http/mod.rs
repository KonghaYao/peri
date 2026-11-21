//! HTTP contract: five `/v1/agents/sessions` routes, auth, errors, and SSE.
//!
//! The router only knows [`SwarmService`] and [`crate::config::SwarmConfig`].
//! It never constructs a fake or branches on implementation type; S4 plugs in
//! the actor-backed service through [`AppState`].

mod auth;
mod error;
mod handlers;
pub mod request;
mod sse;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

use crate::config::SwarmConfig;
use crate::service::SwarmService;

pub const CONTRACT_VERSION: &str = "1";
pub const CONTRACT_HEADER: &str = "x-peri-swarm-contract";
pub const EXPECT_HEADER: &str = "x-peri-swarm-expect";
pub const OPENAI_BETA_HEADER: &str = "openai-beta";

/// Request bodies are capped at the Bun bridge's 8 MiB (`design §2.1`).
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_INSTRUCTIONS_BYTES: usize = 64 * 1024;
pub const MAX_MODEL_BYTES: usize = 256;

#[derive(Clone)]
pub struct AppState {
    service: Arc<dyn SwarmService>,
    config: Arc<SwarmConfig>,
}

impl AppState {
    #[must_use]
    pub fn new(service: Arc<dyn SwarmService>, config: Arc<SwarmConfig>) -> Self {
        Self { service, config }
    }

    #[must_use]
    pub fn service(&self) -> &Arc<dyn SwarmService> {
        &self.service
    }

    #[must_use]
    pub fn config(&self) -> &SwarmConfig {
        &self.config
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bind_addr", &self.config.bind_addr)
            .finish_non_exhaustive()
    }
}

/// Build the gateway router.
///
/// `/healthz` is the one route outside the contract/bearer gate: a supervisor
/// must be able to probe the listener without a deployment secret, and the
/// response is a constant that contains no session fact (S6b).
pub fn build_router(state: AppState) -> Router {
    let sessions = Router::new()
        .route(
            "/v1/agents/sessions",
            get(handlers::list_sessions).post(handlers::create_session),
        )
        .route(
            "/v1/agents/sessions/{logical_id}",
            get(handlers::get_session)
                // Official `sessions.update` is `POST /agents/sessions/{id}`
                // (`sessions.js` in `openai@7.60.0`), not PATCH.
                .post(handlers::update_session)
                .delete(handlers::delete_session),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/events",
            post(handlers::submit_event).get(handlers::stream_events),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/items",
            get(handlers::list_items),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/turns",
            get(handlers::list_turns),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/turns/{turn_id}",
            get(handlers::get_turn),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/turns/{turn_id}/items",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/traces",
            get(handlers::unsupported_resource).post(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/artifacts",
            get(handlers::unsupported_resource).post(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/artifacts/{artifact_id}",
            get(handlers::unsupported_resource).delete(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/artifacts/{artifact_id}/content",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents/{subagent_id}",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents/{subagent_id}/items",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents/{subagent_id}/turns",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents/{subagent_id}/turns/{turn_id}",
            get(handlers::unsupported_resource),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents/{subagent_id}/turns/{turn_id}/items",
            get(handlers::unsupported_resource),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), auth::guard));
    Router::new()
        .route("/healthz", get(handlers::healthz))
        .merge(sessions)
        .with_state(state)
}
