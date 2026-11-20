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
    let initial_input = request::resolve_create_input(&body)?;
    let environment = request::resolve_environment(&body)?;
    let stream = body.stream;
    let command = CreateSessionCommand {
        logical_id: logical_id.clone(),
        cwd,
        model,
        instructions,
        initial_input,
        environment,
        root_session_id: None,
    };

    if stream {
        // Contract: the SSE subscription exists before the first turn runs.
        let subscription = state.service().open_stream(&logical_id).await;
        let result = state.service().create_session(command).await?;
        return Ok(sse::create_response(
            &result,
            subscription,
            state.config().sse_heartbeat,
        ));
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
    let outcome = state
        .service()
        .submit_input(&logical_id, submission)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(outcome)).into_response())
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
