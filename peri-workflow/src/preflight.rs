//! Validation owns its temporary input and process until completion or cancellation.

use serde_json::Value;

/// 用生产解析路径取得引擎后校验脚本。
///
/// 引擎来源与执行路径同 `peri workflow`：随包 sidecar / 本机缓存 / 显式注入（仓库不读
/// 工作树 dist）；缺失或摘要不符时返回 `Workflow preflight unavailable: …`。
pub(super) async fn preflight_validate_script(script: &str) -> Result<(), String> {
    let prepared = crate::runner::prepare_workflow_command()
        .await
        .map_err(|error| format!("Workflow preflight unavailable: {error}"))?;
    validate_script_with(&prepared, script).await
}

/// 以已准备的引擎命令校验脚本（测试注入用；spawn/退出码/错误解码与生产同路径）。
pub(super) async fn validate_script_with(
    prepared: &crate::runner::WorkflowCommand,
    script: &str,
) -> Result<(), String> {
    let temp = tempfile::Builder::new()
        .prefix("peri-workflow-preflight-")
        .tempdir()
        .map_err(unavailable)?;
    let source = temp.path().join("script.js");
    std::fs::write(&source, script).map_err(unavailable)?;

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
