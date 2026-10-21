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
