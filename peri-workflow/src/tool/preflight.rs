//! Validation owns its temporary input and process until completion or cancellation.

use serde_json::Value;

pub(super) async fn preflight_validate_script(script: &str) -> Result<(), String> {
    let temp = tempfile::Builder::new()
        .prefix("peri-workflow-preflight-")
        .tempdir()
        .map_err(unavailable)?;
    let artifact = temp.path().join("peri-workflow.js");
    let source = temp.path().join("script.js");
    std::fs::write(&artifact, crate::runner::WORKFLOW_ARTIFACT_BYTES).map_err(unavailable)?;
    std::fs::write(&source, script).map_err(unavailable)?;

    // Normal completion waits/reaps. Cancellation kills the child and lets Tokio
    // reap it best-effort; TempDir removes both inputs on every exit path.
    let output = tokio::process::Command::new("node")
        .kill_on_drop(true)
        .arg(&artifact)
        .arg("validate")
        .arg(&source)
        .arg("--json")
        .output()
        .await
        .map_err(unavailable)?;
    if output.status.success() {
        return Ok(());
    }
    let message = serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|value| value["errors"].as_array().cloned())
        .and_then(|errors| errors.first().and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "script validation failed".to_string());
    Err(format!("Workflow preflight failed: {message}"))
}

fn unavailable(error: std::io::Error) -> String {
    format!("Workflow preflight unavailable: {error}")
}
