use super::*;
use peri_agent::tools::BaseTool;
use peri_mcp_common::invoke_tool_call;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use serde_json::Value;
use std::sync::Arc;

fn call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

fn complete(response: CallToolResponse) -> CallToolResult {
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("expected Complete result, got {other:?}"),
    }
}

/// Failed writes expose an owned draft reference and the I/O diagnostic, never the draft content.
#[tokio::test]
async fn write_draft_receipt_can_restore_without_resending_content() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().to_string();
    let target = dir.path().join("retry.txt");
    std::fs::create_dir(&target).unwrap();
    let tools: Vec<Arc<dyn BaseTool>> = vec![Arc::new(WriteFileTool::with_draft(&cwd, true))];

    let failed = complete(
        invoke_tool_call(
            &tools,
            &cwd,
            &call(
                "Write",
                serde_json::json!({"file_path":"retry.txt", "content":"private-draft-marker"}),
            ),
        )
        .await
        .unwrap(),
    );
    assert_eq!(failed.is_error, Some(true));
    let text = failed
        .content
        .iter()
        .find_map(|block| match block {
            rmcp::model::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .unwrap();
    assert!(!text.contains("private-draft-marker"));
    assert!(text.contains(&cwd));
    let draft = text
        .split("from_draft=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .trim_end_matches('.');

    std::fs::remove_dir(&target).unwrap();
    let restored = complete(
        invoke_tool_call(
            &tools,
            &cwd,
            &call(
                "Write",
                serde_json::json!({"file_path":"retry.txt", "from_draft":draft}),
            ),
        )
        .await
        .unwrap(),
    );
    assert!(!restored.is_error.unwrap_or(false));
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "private-draft-marker"
    );
}
