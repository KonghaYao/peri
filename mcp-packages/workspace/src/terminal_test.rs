use std::path::Path;
use std::sync::Arc;

use peri_agent::agent::events_v2::EventBus;
use peri_agent::agent::react::Reasoning;
use peri_agent::agent::stages::tool_dispatch::DispatchOutcome;
use peri_agent::agent::stages::StageContext;
use peri_agent::messages::BaseMessage;
use peri_agent::session::{MessageQueue, MessageTranscript};
use peri_agent::tools::BaseTool;
use tokio_util::sync::CancellationToken;

use crate::terminal::BashTool;

#[path = "../../../peri-middlewares/src/at_mention/work_fixture.rs"]
mod work_fixture;

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

#[path = "terminal_process_group_test.rs"]
mod process_group_tests;

#[path = "terminal_test/test_cases.rs"]
mod test_cases;
use test_cases::dispatch_bash;
use test_cases::dispatch_context;
use test_cases::dispatch_context_with_events;
use test_cases::maybe_export_fixture;
use test_cases::output_command;
