use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::error::{from_json_rejection, from_query_rejection};
use super::request::{self, CreateSessionBody, EventsQuery, ItemsQuery, SubmitEventBody};
use super::{sse, AppState};
use crate::domain::{resolve_cwd, CreateSessionResponse, LogicalId, SwarmError};
use crate::service::CreateSessionCommand;

/// Process liveness for the bootstrap entry (S6b). Deliberately constant: it
/// does not touch the store, so a green probe means "the gateway is listening",
/// not "the upstream deployment works".
pub async fn healthz() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

pub async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CreateSessionBody>, JsonRejection>,
) -> Result<Response, SwarmError> {
    let Json(body) = body.map_err(from_json_rejection)?;
    let logical_id = request::resolve_logical_id(&body, &headers)?;
    let cwd = resolve_cwd(
        body.cwd.as_deref(),
        state.config().workspace_root(),
        &logical_id,
        true,
    )?;
    let (model, instructions) = request::resolve_agent(&body)?;
    let mcp_tools = request::resolve_tools(&body)?;
    let initial_input = request::resolve_create_input(&body)?;
    let environment = request::resolve_environment(&body)?;
    let stream = body.stream;
    let mut command = CreateSessionCommand {
        logical_id: logical_id.clone(),
        cwd,
        model,
        instructions,
        mcp_tools,
        initial_input,
        environment: environment.0,
        environment_id: environment.1,
        root_session_id: None,
        defer_initial_turn: false,
    };

    if stream {
        // Contract: the SSE subscription exists before the first turn runs.
        let subscription = state.service().open_stream(&logical_id).await;
        // The receiver is not polled until the response body reaches the
        // client. Running the turn inside `create_session` would buffer every
        // event with no consumer, and a turn longer than the broadcast capacity
        // would surface as `Lagged`: one `gap` frame, then the stream closes
        // with the whole first turn lost. Record the input now (acceptance
        // stays in the service) and answer with headers plus the initial
        // `session` frame before the turn starts.
        command.defer_initial_turn = true;
        let mut result = state.service().create_session(command).await?;
        // Build the response (headers plus the initial `session` frame) before
        // scheduling the turn, so the response never depends on the turn.
        let response = sse::create_response(&result, subscription, state.config().sse_heartbeat);
        // The actor path schedules the deferred turn itself; the in-process
        // fake returns it here. Spawn it before answering, mirroring
        // `submit_event`, so the turn does not wait for the HTTP response.
        if let Some(pending) = result.pending_initial.take() {
            let service = std::sync::Arc::clone(state.service());
            tokio::spawn(async move {
                if let Err(error) = service.run_pending_turn(pending).await {
                    tracing::warn!(
                        error = %error,
                        logical_id = %logical_id,
                        "deferred swarm first turn failed; the stream carries the terminal"
                    );
                }
            });
        }
        return Ok(response);
    }

    let result = state.service().create_session(command).await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let response = CreateSessionResponse::new(result.session, result.created);
    Ok((status, Json(response)).into_response())
}

pub async fn get_session(
    State(state): State<AppState>,
    Path(logical_id): Path<String>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    let session = state.service().get_session(&logical_id).await?;
    Ok(Json(session).into_response())
}

pub async fn submit_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(logical_id): Path<String>,
    body: Result<Json<SubmitEventBody>, JsonRejection>,
) -> Result<Response, SwarmError> {
    let Json(body) = body.map_err(from_json_rejection)?;
    let logical_id = LogicalId::parse(&logical_id)?;
    let submission = request::resolve_submission(body, &headers)?;
    // `202` is acceptance, not durable completion: the response is sent once the
    // receipt, user item, and turn reservation are persisted. The turn itself
    // runs out-of-band so a slow model cannot hold the HTTP response open.
    let accepted = state
        .service()
        .accept_input(&logical_id, submission)
        .await?;
    if let Some(pending) = accepted.pending_turn {
        let service = std::sync::Arc::clone(state.service());
        tokio::spawn(async move {
            if let Err(error) = service.run_pending_turn(pending).await {
                tracing::warn!(
                    error = %error,
                    logical_id = %logical_id,
                    "accepted swarm turn failed; the stream carries the terminal"
                );
            }
        });
    }
    Ok((StatusCode::ACCEPTED, Json(accepted.outcome)).into_response())
}

pub async fn stream_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(logical_id): Path<String>,
    query: Result<Query<EventsQuery>, QueryRejection>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    let Query(query) = query.map_err(from_query_rejection)?;
    let cursor = request::resolve_events_cursor(&query, &headers)?;
    let subscription = state.service().subscribe(&logical_id, cursor).await?;
    Ok(sse::event_response(
        subscription,
        state.config().sse_heartbeat,
    ))
}

pub async fn list_items(
    State(state): State<AppState>,
    Path(logical_id): Path<String>,
    query: Result<Query<ItemsQuery>, QueryRejection>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    let Query(query) = query.map_err(from_query_rejection)?;
    let window = request::resolve_items_window(&query)?;
    let page = state.service().list_items(&logical_id, window).await?;
    Ok(Json(page).into_response())
}
