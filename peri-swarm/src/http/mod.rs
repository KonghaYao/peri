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
mod telemetry;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

use crate::config::SwarmConfig;
use crate::service::SwarmService;
use crate::telemetry::HttpTelemetry;

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
    telemetry: Option<HttpTelemetry>,
}

impl AppState {
    #[must_use]
    pub fn new(service: Arc<dyn SwarmService>, config: Arc<SwarmConfig>) -> Self {
        Self {
            service,
            config,
            telemetry: None,
        }
    }

    /// Attach the HTTP telemetry handle. Without one the router is byte-for-byte
    /// the pre-telemetry router: no middleware, no metric/log side effects.
    #[must_use]
    pub fn with_telemetry(mut self, telemetry: HttpTelemetry) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Attach an optional handle (the process passes whatever `telemetry::init`
    /// produced; tests pass `None` for the disabled path).
    #[must_use]
    pub fn with_optional_telemetry(mut self, telemetry: Option<HttpTelemetry>) -> Self {
        self.telemetry = telemetry;
        self
    }

    #[must_use]
    pub fn service(&self) -> &Arc<dyn SwarmService> {
        &self.service
    }

    #[must_use]
    pub fn config(&self) -> &SwarmConfig {
        &self.config
    }

    #[must_use]
    pub fn telemetry(&self) -> Option<&HttpTelemetry> {
        self.telemetry.as_ref()
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bind_addr", &self.config.bind_addr)
            .field("telemetry", &self.telemetry.is_some())
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
        // Cross-session token report for the Agent metrics page; registered in
        // the auth-guarded router like every other data route.
        .route("/v1/agents/metrics", get(handlers::get_agent_metrics))
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
            get(handlers::list_subagents),
        )
        .route(
            "/v1/agents/sessions/{logical_id}/subagents/{subagent_id}",
            get(handlers::get_subagent),
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
    let layer_state = state.clone();
    let router = Router::new()
        .route("/healthz", get(handlers::healthz))
        .merge(sessions)
        .with_state(state);
    if layer_state.telemetry().is_some() {
        // Outermost layer: it observes unmatched paths and auth rejections too,
        // and it ends at the response headers (the SSE body is never wrapped).
        router.layer(middleware::from_fn_with_state(
            layer_state,
            telemetry::observe,
        ))
    } else {
        router
    }
}
