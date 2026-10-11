use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use peri_agent::agent::react::ToolCall;
use peri_agent::agent::state::AgentState;
use peri_agent::error::AgentError;
use peri_agent::middleware::chain::MiddlewareChain;
use peri_agent::tools::{BaseTool, ToolContext};

use super::*;
use crate::hooks::middleware::HookMiddleware;
use crate::hooks::types::{HookType, RegisteredHook};
use crate::permission::{PermissionMode, SharedPermissionMode};

struct ErrorCapture(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for ErrorCapture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::ERROR
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{}={value:?} ", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
}

#[test]
fn invalid_output_is_diagnosed_without_body() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(ErrorCapture(Arc::clone(&events)));
    let action = parse_command_hook_output(
        &HookEvent::PermissionRequest,
        r#"{"hook_specific_output":{"hookEventName":"secret-unknown-event","secret-body":"deny"}}"#,
    );
    assert!(matches!(action, HookAction::Block { .. }));
    let output = events.lock().unwrap().join("\n");
    assert!(output.contains("PermissionRequest"));
    assert!(output.contains("invalid_command_json"));
    assert!(!output.contains("secret-unknown-event"));
    assert!(!output.contains("secret-body"));
}

#[test]
fn mismatched_event_is_rejected_even_with_top_level_system_message() {
    let output = r#"{"systemMessage":"ignore deny","hook_specific_output":{"hookEventName":"SessionStart","initialUserMessage":"foreign"}}"#;
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, output),
        HookAction::Block { .. }
    ));
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PermissionRequest, output),
        HookAction::Block { .. }
    ));
}

#[test]
fn unsupported_permission_request_specific_output_is_rejected() {
    let output = r#"{"hook_specific_output":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny"}}}"#;
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PermissionRequest, output),
        HookAction::Block { .. }
    ));
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PermissionRequest, output),
        HookAction::Block { .. }
    ));
}

#[test]
fn unknown_fields_cannot_silently_discard_a_decision() {
    for output in [
        r#"{"permissionDecision":"deny"}"#,
        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}"#,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","decision":"deny"}}"#,
    ] {
        assert!(matches!(
            parse_command_hook_output(&HookEvent::PreToolUse, output),
            HookAction::Block { .. }
        ));
    }
}

struct CountingTool(AtomicUsize);

#[async_trait::async_trait]
impl BaseTool for CountingTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "record execution"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _context: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("executed".to_string())
    }
}

fn command_hook(event: HookEvent, output: &str) -> RegisteredHook {
    RegisteredHook {
        event,
        hook: HookType::Command {
            command: format!("printf '%s' '{output}'"),
            shell: None,
            timeout: Some(10),
            status_message: None,
            once: false,
            async_run: false,
            async_rewake: false,
            matcher: None,
            condition: None,
        },
        matcher: None,
        plugin_name: "output-validation".to_string(),
        plugin_id: "output-validation".to_string(),
        plugin_source: None,
        plugin_root: std::env::temp_dir(),
        plugin_data_dir: std::env::temp_dir(),
        plugin_options: HashMap::new(),
    }
}

async fn execute_after_hooks(
    hooks: Vec<RegisteredHook>,
    mode: PermissionMode,
    tool: &CountingTool,
) -> peri_agent::error::AgentResult<ToolCall> {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().unwrap();
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(HookMiddleware::new(
        hooks,
        Arc::new(|| panic!("command hooks do not use a model")),
        cwd,
        "output-validation",
        "",
        SharedPermissionMode::new(mode),
        "opus",
    )));
    let result = chain
        .run_before_tool(
            &mut AgentState::new(cwd),
            ToolCall::new("call", "Bash", serde_json::json!({"command":"test"})),
        )
        .await;
    if let Ok(call) = &result {
        tool.invoke(call.input.clone(), ToolContext::new(&[], cwd))
            .await
            .unwrap();
    }
    result
}

#[cfg(unix)]
#[tokio::test]
async fn pretooluse_deny_then_foreign_output_executes_no_tool() {
    let tool = CountingTool(AtomicUsize::new(0));
    let hooks = vec![
        command_hook(
            HookEvent::PreToolUse,
            r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}"#,
        ),
        command_hook(
            HookEvent::PreToolUse,
            r#"{"hook_specific_output":{"hookEventName":"SessionStart","initialUserMessage":"foreign"}}"#,
        ),
        command_hook(HookEvent::PreToolUse, "{}"),
    ];
    let result = execute_after_hooks(hooks, PermissionMode::Bypass, &tool).await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn unsupported_permission_request_deny_executes_no_tool_and_fires_denied() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("permission-denied");
    let mut watcher = command_hook(HookEvent::PermissionDenied, "{}");
    if let HookType::Command { command, .. } = &mut watcher.hook {
        *command = format!("touch '{}'", marker.display());
    }
    let hooks = vec![
        command_hook(
            HookEvent::PermissionRequest,
            r#"{"hook_specific_output":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny"}}}"#,
        ),
        command_hook(HookEvent::PermissionRequest, "{}"),
        watcher,
    ];
    let tool = CountingTool(AtomicUsize::new(0));
    let result = execute_after_hooks(hooks, PermissionMode::Default, &tool).await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
    assert!(marker.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn valid_no_opinion_output_executes_tool() {
    let tool = CountingTool(AtomicUsize::new(0));
    let result = execute_after_hooks(
        vec![command_hook(HookEvent::PreToolUse, "{}")],
        PermissionMode::Bypass,
        &tool,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(tool.0.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn malformed_pretooluse_output_executes_no_tool() {
    let tool = CountingTool(AtomicUsize::new(0));
    let result = execute_after_hooks(
        vec![command_hook(HookEvent::PreToolUse, "{invalid-json")],
        PermissionMode::Bypass,
        &tool,
    )
    .await;
    assert!(matches!(result, Err(AgentError::ToolRejected { .. })));
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
}
