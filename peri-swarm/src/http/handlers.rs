use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::error::{from_bytes_rejection, from_json_rejection, from_query_rejection};
use super::request::{
    self, CreateSessionBody, EventsQuery, ItemsQuery, SessionsQuery, SubmitEventBody, TurnsQuery,
};
use super::{sse, AppState};
use crate::domain::{resolve_cwd, CreateSessionResponse, LogicalId, SessionDeleted, SwarmError};
use crate::service::{CreateSessionCommand, UpdateField};

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
    let metadata = match &body.metadata {
        UpdateField::Keep => UpdateField::Keep,
        UpdateField::Clear => UpdateField::Clear,
        UpdateField::Set(value) => UpdateField::Set(request::resolve_metadata(value, "metadata")?),
    };
    let prompt_cache_options = match &body.prompt_cache_options {
        UpdateField::Keep => UpdateField::Keep,
        UpdateField::Clear => UpdateField::Clear,
        UpdateField::Set(value) => match request::resolve_prompt_cache_options(value)? {
            Some(options) => UpdateField::Set(options),
            // Only `null` resolves to `None`; `{}` uses the official default
            // TTL (`30m`), not "clear".
            None => UpdateField::Clear,
        },
    };
    let initial_input = request::resolve_create_input(&body)?;
    let environment = request::resolve_environment(&body)?;
    let stream = body.stream;
    let mut command = CreateSessionCommand {
        logical_id: logical_id.clone(),
        cwd,
        model,
        instructions,
        mcp_tools,
        metadata,
        prompt_cache_options,
        initial_input,
        environment: environment.0,
        environment_id: environment.1,
        root_session_id: None,
        defer_initial_turn: false,
    };

    if stream {
        // Contract: the SSE subscription exists before the first turn is
        // scheduled. The actor may start the turn right after its action reply,
        // so delivery is bounded by the broadcast capacity, not by this
        // response.
        let subscription = state.service().open_stream(&logical_id).await;
        // The receiver is not polled until the response body reaches the
        // client. Running the turn inside `create_session` would buffer every
        // event with no consumer, and a turn longer than the broadcast capacity
        // would surface as `Lagged`: one `gap` frame, then the stream closes
        // with the whole first turn lost. Record the input now (acceptance
        // stays in the service) and answer with headers plus the initial
        // `session` frame without waiting for the turn. Events that exceed the
        // capacity while the body is unpolled fail with `gap` + EOF; the client
        // reconnects from the last rendered item `seq`.
        command.defer_initial_turn = true;
        let mut result = state.service().create_session(command).await?;
        // Build the response (headers plus the initial `session` frame) from
        // the accepted result; it never waits for the turn to finish.
        let response = sse::create_response(&result, subscription, state.config().sse_heartbeat);
        // The actor path schedules the deferred turn itself (`ctx.wait_until`)
        // and returns no pending turn across the process boundary. The
        // in-process fake returns it here. Spawn it before answering, mirroring
        // `submit_event`; the in-process turn does not wait for the response,
        // whereas the actor's turn may already have started.
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

pub async fn list_sessions(
    State(state): State<AppState>,
    query: Result<Query<SessionsQuery>, QueryRejection>,
) -> Result<Response, SwarmError> {
    let Query(query) = query.map_err(from_query_rejection)?;
    let window = request::resolve_sessions_window(&query)?;
    let page = state.service().list_sessions(window).await?;
    Ok(Json(page).into_response())
}

pub async fn update_session(
    State(state): State<AppState>,
    Path(logical_id): Path<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    let bytes = body.map_err(from_bytes_rejection)?;
    let Some(body) = request::parse_update_body(&bytes, &headers)? else {
        // The official SDK sends no body and no content-type for
        // `update(id)` / `update(id, null)`. An empty update has nothing to
        // apply, so answer with the current projection and write nothing
        // (design §4.1) instead of running the in-flight admission.
        let session = state.service().get_session(&logical_id).await?;
        return Ok(Json(session).into_response());
    };
    let command = request::resolve_update(body)?;
    let session = state.service().update_session(&logical_id, command).await?;
    Ok(Json(session).into_response())
}

pub async fn delete_session(
    State(state): State<AppState>,
    Path(logical_id): Path<String>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    state.service().delete_session(&logical_id).await?;
    Ok(Json(SessionDeleted::new(logical_id.as_str())).into_response())
}

pub async fn list_turns(
    State(state): State<AppState>,
    Path(logical_id): Path<String>,
    query: Result<Query<TurnsQuery>, QueryRejection>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    let Query(query) = query.map_err(from_query_rejection)?;
    let window = request::resolve_turns_window(&query)?;
    let page = state.service().list_turns(&logical_id, window).await?;
    Ok(Json(page).into_response())
}

pub async fn get_turn(
    State(state): State<AppState>,
    Path((logical_id, turn_id)): Path<(String, String)>,
) -> Result<Response, SwarmError> {
    let logical_id = LogicalId::parse(&logical_id)?;
    let turn = state.service().get_turn(&logical_id, &turn_id).await?;
    Ok(Json(turn).into_response())
}

/// Registered official resource the gateway cannot map.
///
/// Registering the route (instead of letting axum answer) keeps the error a
/// gateway JSON envelope and still runs the Bearer gate.
pub async fn unsupported_resource(uri: Uri) -> Result<Response, SwarmError> {
    Err(SwarmError::unsupported_resource(official_resource(
        uri.path(),
    )))
}

fn official_resource(path: &str) -> &'static str {
    let subagent_turns =
        path.contains("/subagents/") && (path.contains("/turns/") || path.ends_with("/turns"));
    if path.contains("/traces") {
        "sessions.traces"
    } else if path.contains("/artifacts") && path.ends_with("/content") {
        "sessions.artifacts.content"
    } else if path.contains("/artifacts") {
        "sessions.artifacts"
    } else if subagent_turns && path.ends_with("/items") {
        "sessions.subagents.turns.items"
    } else if subagent_turns {
        "sessions.subagents.turns"
    } else if path.contains("/subagents/") && path.ends_with("/items") {
        "sessions.subagents.items"
    } else if path.contains("/subagents") {
        "sessions.subagents"
    } else if path.contains("/turns/") && path.ends_with("/items") {
        "sessions.turns.items"
    } else {
        "unknown"
    }
}

pub async fn submit_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(logical_id): Path<String>,
    body: Result<Json<SubmitEventBody>, JsonRejection>,
) -> Result<Response, SwarmError> {
    let Json(body) = body.map_err(from_json_rejection)?;
    let logical_id = LogicalId::parse(&logical_id)?;
    let submission = match request::resolve_event(body, &headers)? {
        request::SubmitEvent::Input(submission) => submission,
        request::SubmitEvent::Approval(answer) => {
            // The answer goes back to the still-suspended `session/prompt`; it
            // never creates or queues a turn.
            let receipt = state
                .service()
                .respond_required_action(
                    &logical_id,
                    &answer.request_id,
                    answer.decision,
                    answer.input_id,
                )
                .await?;
            return Ok((StatusCode::ACCEPTED, Json(receipt)).into_response());
        }
    };
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
