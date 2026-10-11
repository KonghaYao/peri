//! Validation owns its temporary input and process until completion or cancellation.

use serde_json::Value;

pub(super) async fn preflight_validate_script(script: &str) -> Result<(), String> {
    let temp = tempfile::Builder::new()
        .prefix("peri-workflow-preflight-")
        .tempdir()
        .map_err(unavailable)?;
    let source = temp.path().join("script.js");
    std::fs::write(&source, script).map_err(unavailable)?;

    let prepared = crate::runner::prepare_workflow_command()
        .await
        .map_err(|error| format!("Workflow preflight unavailable: {error}"))?;

    // Normal completion waits/reaps. Cancellation kills the child and lets Tokio
    // reap it best-effort; TempDir removes the input on every exit path.
    let mut command = tokio::process::Command::new(&prepared.program);
    command
        .kill_on_drop(true)
        .args(&prepared.args)
        .arg("validate")
        .arg(&source)
        .arg("--json");
    let output = crate::process::output(command).await.map_err(unavailable)?;
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
