//! `session/execute-command` dispatch handler.
//!
//! Accepts a slash command string and delegates to the registered
//! [`AgentCommand`] implementations in [`crate::session::command`].
//! This mirrors the in-process interception done by
//! [`crate::session::executor::intercept_immediate_command`] but exposes
//! it as a standalone ACP JSON-RPC method so that external clients (IDE,
//! stdio transport) can execute Immediate commands without going through
//! the full `session/prompt` pipeline.

use serde_json::Value;

use crate::session::command::{
    default_command_registry, CommandContext, CommandKind, CommandResult,
};
use crate::session::executor::PromptStopReason;
use crate::transport::types::AcpError;

/// Execute a slash command against the given session.
///
/// Accepts `{ session_id, command, args }` in `params`, looks up the command
/// in the default [`CommandRegistry`], and if it is an `Immediate` command,
/// runs it synchronously (blocking the caller) and returns the updated
/// message list.
///
/// # Errors
///
/// Returns `AcpError` when:
/// - `session_id` is missing
/// - `command` is missing
/// - The command string does not match any registered command
/// - The matched command is not `Immediate` (Passthrough/Transform commands
///   must go through `session/prompt`)
#[allow(clippy::too_many_arguments)]
pub async fn execute_command(
    params: &Value,
    session_history: Vec<peri_agent::messages::BaseMessage>,
    cwd: &str,
    peri_config: &std::sync::Arc<crate::provider::PeriConfig>,
    event_sink: &std::sync::Arc<dyn crate::session::event_sink::EventSink>,
    auxiliary_model: Option<std::sync::Arc<dyn peri_model::Model>>,
    cancel_token: &peri_agent::agent::AgentCancellationToken,
    thread_store: Option<std::sync::Arc<dyn peri_agent::thread::ThreadStore>>,
    thread_id: Option<String>,
    bg_event_tx: Option<
        tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>,
    >,
    bg_registry: Option<std::sync::Arc<peri_middlewares::subagent::BackgroundTaskRegistry>>,
    frozen_claude_md: Option<std::sync::Arc<String>>,
    frozen_claude_local_md: Option<std::sync::Arc<String>>,
    frozen_skill_summary: Option<std::sync::Arc<String>>,
    frozen_system_prompt: Option<std::sync::Arc<String>>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let command_str = params
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing command"))?;

    let args_value = params.get("args").cloned().unwrap_or(Value::Null);

    // Convert args Value to a plain string representation.
    // For JSON object args (e.g. rewind), pass the JSON string.
    // For simple string args, pass as-is.
    let args_string = match args_value {
        Value::Null => String::new(),
        Value::String(s) => s,
        other => other.to_string(),
    };

    let registry = default_command_registry();
    let (cmd, _cmd_args) = registry
        .find(command_str)
        .ok_or_else(|| AcpError::new(-32602, format!("unknown command: {command_str}")))?;

    if cmd.kind() != CommandKind::Immediate {
        return Err(AcpError::new(
            -32602,
            format!(
                "command '{}' is not Immediate; use session/prompt instead",
                cmd.name()
            ),
        ));
    }

    let cancel_history = session_history.clone();
    let ctx = CommandContext {
        session_id: session_id.clone(),
        history: session_history,
        cwd: cwd.to_string(),
        peri_config: std::sync::Arc::clone(peri_config),
        auxiliary_model,
        event_sink: std::sync::Arc::clone(event_sink),
        args: args_string,
        cancel_token: cancel_token.clone(),
        thread_store,
        thread_id,
        bg_event_sender: bg_event_tx,
        bg_registry,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        frozen_system_prompt,
    };

    let result = tokio::select! {
        r = cmd.execute(ctx) => r,
        _ = cancel_token.cancelled() => {
            tracing::info!(session_id = %session_id, "execute_command: cancelled");
            CommandResult {
                messages: cancel_history,
                stop_reason: PromptStopReason::Cancelled,
            }
        }
    };

    // Immediate command bypasses the agent event pump, so we must manually
    // signal completion. Otherwise the TUI stays in loading state.
    // [TRAP] See issue_2026-05-29-immediate-command-missing-push-done.
    event_sink.push_done(&session_id, "end_turn").await;

    // Serialize the result messages into a compact JSON array of { role, content }.
    let messages_json: Vec<Value> = result
        .messages
        .iter()
        .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
        .collect();

    Ok(serde_json::json!({
        "messages": messages_json,
        "stop_reason": format!("{:?}", result.stop_reason),
    }))
}

/// Extract and validate the required parameters for `session/execute-command`.
///
/// Returns `(session_id, command, args)` on success.
/// This is a lightweight extraction that does **not** execute the command.
pub fn extract_execute_command_params(params: &Value) -> Result<(String, String, Value), AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();

    let command = params
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing command"))?
        .to_string();

    let args = params.get("args").cloned().unwrap_or(Value::Null);

    Ok((session_id, command, args))
}

#[cfg(test)]
#[path = "execute_command_test.rs"]
mod tests;
