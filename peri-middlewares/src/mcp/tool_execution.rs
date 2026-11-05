//! Execution evidence at the trusted builtin MCP boundary. Tool text is never parsed.
use peri_acp_types::tool_outcome::RawToolResult;
use peri_agent::tools::{BaseTool, ToolExecutionEvidence, ToolExecutionStatus, ToolOutput};

fn evidence(status: ToolExecutionStatus, task_id: Option<String>) -> ToolExecutionEvidence {
    ToolExecutionEvidence {
        status,
        task_id,
        exit_code: None,
        output_ref: None,
        output_truncated: false,
    }
}

pub(super) fn task_started(public_id: String, delivery_note: &str) -> ToolOutput {
    ToolOutput::with_execution(
        format!("Background task started: {public_id}. {delivery_note}"),
        evidence(ToolExecutionStatus::Running, Some(public_id)),
    )
}

/// Monitor receipt: the host public task id and the monitor-side task id are
/// different namespaces. `monitor_id` is what the monitor tools and MCP
/// `tasks/get` accept; the host `task_id` belongs to host task management and
/// the Tasks panel only.
pub(super) fn monitor_started(public_id: String, monitor_id: &str) -> ToolOutput {
    ToolOutput::with_execution(
        format!(
            "Background task started: {public_id}. task_id: {public_id} (host background task id; \
             only for host task management and the Tasks panel). monitor_id: {monitor_id} (use this \
             as task_id for monitor_read, monitor_stop, and MCP tasks/get; monitor_list also reports \
             it). Do not pass the host task_id to the monitor tools. The initiating session is woken \
             exactly once when the task reaches a terminal state (completion, failure, \
             cancellation, or deadline); alert matches also wake it while the task runs. Read \
             progress with monitor_read."
        ),
        evidence(ToolExecutionStatus::Running, Some(public_id)),
    )
}

pub(super) fn complete(
    tool: &dyn BaseTool,
    text: String,
    raw: RawToolResult,
) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
    let declaration = tool
        .builtin_mcp_instance()
        .and_then(peri_acp_types::builtin_mcp::find)
        .and_then(|instance| {
            instance
                .tools
                .iter()
                .find(|declaration| Some(declaration.original_name) == tool.mcp_tool_name())
        });
    let execution = if let Some(declaration) = declaration {
        match raw
            .meta()
            .and_then(|meta| meta.get(peri_acp_types::tools::TOOL_EXECUTION_META_KEY))
        {
            Some(value) => Some(serde_json::from_value::<ToolExecutionEvidence>(
                value.clone(),
            )?),
            None if declaration.completes_on_response => Some(evidence(
                if raw.is_error() {
                    ToolExecutionStatus::Failed
                } else {
                    ToolExecutionStatus::Completed
                },
                None,
            )),
            None => None,
        }
    } else {
        None
    };
    let mut output = ToolOutput::from_raw_result(text, raw);
    output.execution = execution;
    Ok(output)
}

#[cfg(test)]
#[path = "tool_execution_test.rs"]
mod tests;
