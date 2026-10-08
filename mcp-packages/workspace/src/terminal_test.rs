use std::time::Instant;
use std::{path::Path, sync::Arc};

use peri_acp_types::event::BackgroundTaskResult;
use peri_agent::agent::events_v2::EventBus;
use peri_agent::agent::stages::StageContext;
use peri_agent::messages::BaseMessage;
use peri_agent::session::{MessageQueue, MessageTranscript};
use peri_agent::tools::{BaseTool, ToolExecutionStatus};
use peri_agent::{agent::react::Reasoning, agent::stages::tool_dispatch::DispatchOutcome};
use tokio_util::sync::CancellationToken;

use super::*;

#[path = "../../../peri-middlewares/src/at_mention/work_fixture.rs"]
mod work_fixture;

struct DispatchLlm(serde_json::Value);

#[derive(Clone)]
struct DispatchFixture {
    context: StageContext,
    directory: Arc<parking_lot::Mutex<Option<tempfile::TempDir>>>,
}

impl std::ops::Deref for DispatchFixture {
    type Target = StageContext;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

#[async_trait::async_trait]
impl peri_agent::agent::react::ReactLLM for DispatchLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<peri_agent::agent::react::StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        Ok(Reasoning::with_tools(
            "run Bash",
            vec![peri_agent::agent::react::ToolCall::new(
                "bash-call",
                "Bash",
                self.0.clone(),
            )],
        ))
    }
}

// Exercise the real platform shell without relying on Python availability or
// PowerShell's native-executable argument quoting. Tail is fixture-owned text.
fn output_command(count: usize, tail: &str, exit_code: i32) -> String {
    if cfg!(windows) {
        format!("[Console]::Out.WriteLine(('x' * {count}) + '{tail}'); exit {exit_code}")
    } else {
        format!("printf '%*s' {count} '' | tr ' ' x; printf '%s\\n' '{tail}'; exit {exit_code}")
    }
}

fn dispatch_context(cwd: &Path, tool: BashTool) -> DispatchFixture {
    let turn = peri_agent::session::turn::TurnContext::new(
        Arc::from(cwd.to_string_lossy().into_owned()),
        Arc::new(CancellationToken::new()),
    );
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let tools = Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::from(
        [("Bash".to_string(), Arc::new(tool) as Arc<dyn BaseTool>)],
    )));
    let context = StageContext::builder(turn, transcript, MessageQueue::new())
        .with_tools(tools)
        .build();
    DispatchFixture {
        context,
        directory: Arc::new(parking_lot::Mutex::new(None)),
    }
}

fn dispatch_context_with_events(
    cwd: &Path,
    tool: BashTool,
) -> (DispatchFixture, peri_acp_types::event_v2::EventHandles) {
    let (bus, handles) = EventBus::new(Default::default());
    let mut context = dispatch_context(cwd, tool);
    context.context.runtime.event_bus = Arc::new(bus);
    (context, handles)
}

async fn dispatch_bash(
    context: &DispatchFixture,
    input: serde_json::Value,
    cancel: CancellationToken,
) -> Result<DispatchOutcome, peri_agent::error::AgentError> {
    let mut stage = context.context.clone();
    let directory = work_fixture::bind(&mut stage).await;
    *context.directory.lock() = Some(directory);
    let mut context = stage;
    context
        .session
        .queue
        .push(peri_agent::session::QueuedMessage::prompt(
            peri_agent::session::MessageSource::UserInput,
            BaseMessage::human("exercise Bash execution evidence"),
        ));
    context.runtime.llm = Arc::new(DispatchLlm(input));
    peri_agent::agent::stages::receive::run_receive(peri_agent::agent::stages::ReceiveInput {
        context: context.clone(),
    })
    .await?;
    let reasoned =
        peri_agent::agent::stages::reason::run_reason(peri_agent::agent::stages::ReasonInput {
            context: context.clone(),
            has_tool_calls: false,
        })
        .await?;
    peri_agent::agent::stages::tool_dispatch::dispatch_tools(
        &context,
        &reasoned.reasoning,
        &reasoned.catalog,
        &cancel,
    )
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
