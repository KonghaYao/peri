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

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/agents/sessions", post(handlers::create_session))
        .route(
            "/v1/agents/sessions/{logical_id}",
            get(handlers::get_session),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/events",
            post(handlers::submit_event).get(handlers::stream_events),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/items",
            get(handlers::list_items),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), auth::guard))
        .with_state(state)
}
