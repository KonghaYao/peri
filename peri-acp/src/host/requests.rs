//! ACP Request dispatch — handles all ACP protocol request methods.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;
use std::sync::Arc;

use crate::session::event_sink::TransportEventSink;
use peri_acp_types::session_resources::{
    ControlAction, ControlCommand, ControlDecision, ControlReceipt, ControlResolution,
};
use peri_acp_types::tasks::BgRegistryEvent;
use serde_json::json;
use serde_json::Value;
use std::sync::atomic::Ordering;

use crate::transport::types::AcpError;
#[cfg(test)]
use peri_acp_types::PeriCaps;

use super::{AcpServerConfig, SessionState};

pub(crate) mod acp_mcp;
pub(crate) mod config_options;
mod mcp_oauth;
mod plugin;
pub(crate) mod resource_owners;
mod rewind;
mod session_control;
pub(crate) mod session_lifecycle;
mod storage_v2;
mod user_input;
mod workflow;

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(Value::as_str);
    let reopening = matches!(method, "session/control" | "session/control/resolve")
        && params
            .get("action")
            .and_then(|action| action.get("kind"))
            .and_then(Value::as_str)
            == Some("reopen");
    let lifecycle = reopening
        || matches!(
            method,
            "session/new"
                | "session/load"
                | "session/resume"
                | "session/fork"
                | "session/close"
                | "session/delete"
        );
    let environment = session_id
        .and_then(|id| sessions.get(id))
        .and_then(|state| state.environment.clone());
    let cfg = if lifecycle {
        cfg
    } else {
        environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg)
    };
    if matches!(
        method,
        "session/rewind"
            | "session/set_mode"
            | "session/set_config_option"
            | "session/rename"
            | "workflow/resume"
            | "workflow/kill_agent"
            | "workflow/kill_run"
            | "session/cancel-bg-task"
            | "session/input/enqueue"
            | "session/input/dispatch"
            | "session/input/takeback"
    ) {
        let id = session_id.ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
        if let Some(state) = sessions.get(id) {
            if state.closing {
                return Err(AcpError::new(-32010, "Session is closing"));
            }
        } else if method != "session/rename" {
            return Err(AcpError::new(-32602, "session not found"));
        }
    }
    if method == "session/rename" && params.get("title").and_then(Value::as_str).is_none() {
        return Err(AcpError::new(-32602, "missing title"));
    }
    if method == "workflow/resume" {
        super::workspace::validate_expected(cfg, session_id.expect("checked"), None).await?;
    }
    let result = match method {
        "initialize" => session_lifecycle::handle_initialize(params, cfg),
        "session/new" => session_lifecycle::handle_new(params, cfg, sessions).await,
        "session/work/query" => super::execution::query(params, cfg).await,
        "session/work/resolve" => super::execution::resolve_work(params, cfg).await,
        "session/execute/resolve" => super::execution::resolve(params, cfg).await,
        "session/control" | "session/control/resolve" | "session/control/state" => {
            session_control::handle(method, params, cfg, sessions).await
        }
        "session/set_mode" => config_options::handle_set_mode(params, cfg, transport).await,
        "session/set_config_option" => {
            config_options::handle_set_config_option(params, cfg, sessions, transport).await
        }
        "session/load" => session_lifecycle::handle_load(params, cfg, sessions, transport).await,
        "session/list" => session_lifecycle::handle_list(params, cfg).await,
        "peri/machines/list" => storage_v2::machines(cfg).await,
        "peri/workspaces/list" => storage_v2::workspaces(params, cfg).await,
        "peri/session/archive" => storage_v2::archive_session(params, cfg).await,
        "peri/session_context" => session_lifecycle::handle_context(params, cfg).await,
        "session/metadata" => session_lifecycle::handle_metadata(params, cfg, false).await,
        "peri/session_history" => session_lifecycle::handle_metadata(params, cfg, true).await,
        "session/input/enqueue"
        | "session/input/dispatch"
        | "session/input/takeback"
        | "session/input/snapshot" => {
            user_input::handle_user_input(method, params, cfg, sessions, transport).await
        }
        "workflow/list_runs" => workflow::handle_list_runs(params, sessions),
        "workflow/kill_agent" => workflow::handle_kill_agent(params, sessions).await,
        "workflow/kill_run" => workflow::handle_kill_run(params, sessions),
        "workflow/resume" => workflow::handle_resume(params, sessions).await,
        "session/cancel-bg-task" => session_lifecycle::handle_cancel_bg_task(params, cfg).await,
        "session/bg-tasks" => session_lifecycle::handle_bg_tasks(params, cfg),
        "session/close" => session_lifecycle::handle_close(params, cfg, sessions).await,
        "session/delete" => session_lifecycle::handle_delete(params, cfg, sessions).await,
        "session/resume" => {
            session_lifecycle::handle_resume(params, cfg, sessions, transport).await
        }
        "session/fork" => session_lifecycle::handle_fork(params, cfg, sessions, transport).await,
        "session/update_config" => {
            config_options::handle_update_config(params, cfg, sessions, transport).await
        }
        "plugin/install" => plugin::handle_install(params, cfg, sessions, transport).await,
        "plugin/uninstall" => plugin::handle_uninstall(params, cfg, sessions, transport).await,
        "plugin/toggle" => plugin::handle_toggle(params, cfg, transport).await,
        "plugin/search" => plugin::handle_search(params, cfg, transport).await,
        "plugin/list" => plugin::handle_session_snapshot(cfg),
        "plugin/update" => plugin::handle_update(params, cfg, transport).await,
        "session/rename" => session_lifecycle::handle_rename(params, cfg, transport).await,
        "session/rewind-candidates" => rewind::handle_rewind_candidates(params, cfg, sessions),
        "session/rewind-preview" => rewind::handle_rewind_preview(params, cfg, sessions).await,
        "session/rewind" => rewind::handle_rewind(params, cfg, sessions, transport).await,
        "marketplace/refresh" => plugin::handle_refresh(params, cfg).await,
        "mcp/list" => mcp_oauth::handle_list(params, cfg),
        "mcp/oauth_start" => mcp_oauth::handle_oauth_start(params, cfg),
        "mcp/oauth_callback" => mcp_oauth::handle_oauth_callback(params, cfg),
        "mcp/oauth_cancel" => mcp_oauth::handle_oauth_cancel(params, cfg),
        _ => Err(AcpError::new(-32601, format!("Method not found: {method}"))),
    };
    if matches!(
        method,
        "session/new" | "session/load" | "session/resume" | "session/fork"
    ) {
        if let Ok(value) = &result {
            let id = value.get("sessionId").and_then(Value::as_str).or_else(|| {
                matches!(method, "session/load" | "session/resume")
                    .then(|| params.get("sessionId").and_then(Value::as_str))
                    .flatten()
            });
            if let Some(id) = id {
                let local = sessions
                    .get(id)
                    .and_then(|state| state.environment.as_ref())
                    .map(|environment| &environment.cfg)
                    .unwrap_or(cfg);
                bind_session_tasks(id, local, transport);
            }
        }
    }
    if matches!(method, "session/control" | "session/control/resolve")
        && serde_json::from_value::<ControlCommand>(params.clone())
            .is_ok_and(|command| command.action == ControlAction::Reopen)
    {
        if let Ok(value) = &result {
            let receipt = if method == "session/control" {
                serde_json::from_value::<ControlReceipt>(value.clone()).ok()
            } else {
                match serde_json::from_value::<ControlResolution>(value.clone()).ok() {
                    Some(ControlResolution::Applied { receipt }) => Some(receipt),
                    _ => None,
                }
            };
            if let Some(receipt) =
                receipt.filter(|receipt| receipt.decision == ControlDecision::Accepted)
            {
                let id = receipt.session_id.as_str();
                if sessions.contains_key(id)
                    && cfg.session_manager.get_session(id).is_some_and(|session| {
                        session.recipient_lifecycle == receipt.state.lifecycle
                    })
                {
                    let local = sessions
                        .get(id)
                        .and_then(|state| state.environment.as_ref())
                        .map(|environment| &environment.cfg)
                        .unwrap_or(cfg);
                    bind_session_tasks(id, local, transport);
                }
            }
        }
    }
    result
}

fn bind_session_tasks(
    session_id: &str,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) {
    let Some(session) = cfg.session_manager.get_session(session_id) else {
        return;
    };
    let manager = Arc::clone(&session.task_manager);
    let pool = cfg.mcp_pool.clone();
    if let Some(pool) = pool.as_ref() {
        pool.bind_session_task_manager(session_id, &manager);
    }
    if session.task_events_started.swap(true, Ordering::AcqRel) {
        return;
    }
    let cancel = session.task_events_cancel.clone();
    let mut changes = manager.subscribe_events();
    let sink = TransportEventSink::new(Arc::clone(transport), cfg.session_manager.caps_registry());
    let id = session_id.to_owned();
    drop(session);
    tokio::spawn(async move {
        if let Some(pool) = pool.as_ref() {
            let _ = peri_time::timeout(
                std::time::Duration::from_millis(500),
                Arc::clone(pool).recover_workspace_tasks(&id),
            )
            .await;
        }
        let snapshot = manager.snapshot();
        let mut revision = snapshot.revision;
        let watcher = pool.map(|pool| {
            let id = id.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { pool.watch_workspace_tasks(&id, cancel).await })
        });
        let _ = sink
            .push_unstable_event(
                &id,
                "bg-task-snapshot".into(),
                task_snapshot_value(snapshot),
            )
            .await;
        loop {
            let change = tokio::select! {
                _ = cancel.cancelled() => break,
                change = changes.recv() => change,
            };
            match change {
                Ok(change) => {
                    if change.revision <= revision {
                        continue;
                    }
                    if change.revision != revision.saturating_add(1) {
                        let snapshot = manager.snapshot();
                        revision = snapshot.revision;
                        let _ = sink
                            .push_unstable_event(
                                &id,
                                "bg-task-snapshot".into(),
                                task_snapshot_value(snapshot),
                            )
                            .await;
                        continue;
                    }
                    revision = change.revision;
                    let (event, data) = match change.event {
                        BgRegistryEvent::Started {
                            task_id,
                            kind,
                            summary,
                            started_at,
                        } => (
                            "bg-task-started",
                            json!({"task_id":task_id,"kind":kind,"summary":summary,"started_at":started_at,"revision":change.revision}),
                        ),
                        BgRegistryEvent::Completed {
                            task_id,
                            kind,
                            success,
                            output_preview,
                            duration_ms,
                            ..
                        } => (
                            "bg-task-completed",
                            json!({"task_id":task_id,"kind":kind,"success":success,"output_preview":output_preview,"duration_ms":duration_ms,"revision":change.revision}),
                        ),
                        BgRegistryEvent::Cancelled { task_id, reason } => (
                            "bg-task-cancelled",
                            json!({"task_id":task_id,"reason":reason,"revision":change.revision}),
                        ),
                        BgRegistryEvent::Updated { task_id, status } => (
                            "bg-task-updated",
                            json!({"task_id":task_id,"status":status,"revision":change.revision}),
                        ),
                    };
                    let _ = sink.push_unstable_event(&id, event.into(), data).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let snapshot = manager.snapshot();
                    revision = snapshot.revision;
                    let _ = sink
                        .push_unstable_event(
                            &id,
                            "bg-task-snapshot".into(),
                            task_snapshot_value(snapshot),
                        )
                        .await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        if let Some(watcher) = watcher {
            watcher.abort();
        }
    });
}

pub(super) fn task_snapshot_value(snapshot: peri_acp_types::tasks::TaskSnapshot) -> Value {
    json!({
        "revision": snapshot.revision,
        "tasks": snapshot.tasks.into_iter()
            .map(|task| json!({
                "task_id":task.task_id, "kind":task.kind, "summary":task.summary,
                "started_at":task.started_at, "status":task.status,
                "duration_ms":task.duration_ms, "output_preview":task.output_preview,
            })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "requests_test.rs"]
mod tests;
