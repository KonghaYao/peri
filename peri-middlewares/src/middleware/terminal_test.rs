use std::time::Instant;
use std::{path::Path, sync::Arc};

#[cfg(unix)]
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::agent::events_v2::EventBus;
use peri_agent::agent::stages::StageContext;
use peri_agent::messages::BaseMessage;
use peri_agent::session::{MessageQueue, MessageTranscript};
use peri_agent::tools::{BaseTool, ToolExecutionStatus};
use peri_agent::{agent::react::Reasoning, agent::stages::tool_dispatch::DispatchOutcome};
use tokio_util::sync::CancellationToken;

use super::*;

// Exercise the real platform shell without relying on Python availability or
// PowerShell's native-executable argument quoting. Tail is fixture-owned text.
fn output_command(count: usize, tail: &str, exit_code: i32) -> String {
    if cfg!(windows) {
        format!("[Console]::Out.WriteLine(('x' * {count}) + '{tail}'); exit {exit_code}")
    } else {
        format!("printf '%*s' {count} '' | tr ' ' x; printf '%s\\n' '{tail}'; exit {exit_code}")
    }
}

fn dispatch_context(cwd: &Path, tool: BashTool) -> StageContext {
    let turn = peri_agent::session::turn::TurnContext::new(
        Arc::from(cwd.to_string_lossy().into_owned()),
        Arc::new(CancellationToken::new()),
    );
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let context = StageContext::new(turn, transcript, MessageQueue::new());
    context
        .runtime
        .tools
        .write()
        .insert("Bash".to_string(), Arc::new(tool) as Arc<dyn BaseTool>);
    context
}

fn dispatch_context_with_events(
    cwd: &Path,
    tool: BashTool,
) -> (StageContext, peri_acp_types::event_v2::EventHandles) {
    let turn = peri_agent::session::turn::TurnContext::new(
        Arc::from(cwd.to_string_lossy().into_owned()),
        Arc::new(CancellationToken::new()),
    );
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let (bus, handles) = EventBus::new(Default::default());
    let context = StageContext::builder(turn, transcript, MessageQueue::new())
        .with_event_bus(Arc::new(bus))
        .build();
    context
        .runtime
        .tools
        .write()
        .insert("Bash".to_string(), Arc::new(tool) as Arc<dyn BaseTool>);
    (context, handles)
}

async fn dispatch_bash(
    context: &StageContext,
    input: serde_json::Value,
    cancel: CancellationToken,
) -> Result<DispatchOutcome, peri_agent::error::AgentError> {
    let catalog = context
        .runtime
        .tool_catalog
        .pin_working_tools(&context.runtime.tools.read())
        .unwrap();
    let reasoning = Reasoning::with_tools(
        "run Bash",
        vec![peri_agent::agent::react::ToolCall::new(
            "bash-call",
            "Bash",
            input,
        )],
    );
    peri_agent::agent::stages::tool_dispatch::dispatch_tools(context, &reasoning, &catalog, &cancel)
        .await
}

fn maybe_export_fixture(messages: &[BaseMessage]) {
    let Ok(path) = std::env::var("PERI_EVIDENCE_FIXTURE_OUT") else {
        return;
    };
    let encoded = serde_json::to_string_pretty(messages).unwrap();
    std::fs::write(path, encoded).unwrap();
}

#[path = "terminal_lifecycle_test.rs"]
mod lifecycle_tests;

#[path = "terminal_evidence_test.rs"]
mod evidence_tests;

#[path = "terminal_basics_test.rs"]
mod basics_tests;

#[path = "terminal_background_test.rs"]
mod background_tests;

#[path = "terminal_diagnostics_test.rs"]
mod diagnostics_tests;
