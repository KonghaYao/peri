//! 固定 Workflow artifact 的身份校验、原子发布、安装与命令准备。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use crate::error::WorkflowError;

/// 本地固定安装的 workflow engine 版本（与 npm 发布版本保持一致）。
/// npx 兜底必须带显式版本：`npx -y @peri-code/workflow` 在全局已有同名 bin
/// 时会静默复用旧版（CLI 子命令缺失、无任何输出），显式 `@<version>` 才能
/// 绕过该行为强制使用 registry 上的目标版本。
const WORKFLOW_NPM_VERSION: &str = "0.2.0";
const WORKFLOW_PACKAGE_NAME: &str = "@peri-code/workflow";
const WORKFLOW_ENTRY: &str = "dist/peri-workflow.js";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(90);
pub(super) const WORKFLOW_PROTOCOL_VERSION: u32 = 1;
pub(super) const WORKFLOW_BUILD_ID: &str = "@peri-code/workflow@0.2.0";
const NPX_FALLBACK_ENV: &str = "PERI_WORKFLOW_ALLOW_NPX_FALLBACK";
pub(crate) const WORKFLOW_ARTIFACT_BYTES: &[u8] =
    include_bytes!("../../../npm-packages/@peri-workflow/dist/peri-workflow.js");

/// 串行化本地安装（避免并发 workflow 同时触发安装）。
static INSTALL_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Debug)]
pub(super) struct WorkflowCommand {
    pub(super) program: String,
    pub(super) args: Vec<String>,
}

#[derive(serde::Deserialize)]
struct WorkflowPackageMetadata {
    name: String,
    version: String,
    main: String,
    #[serde(rename = "periProtocolVersion")]
    protocol_version: u32,
    #[serde(rename = "periBuildId")]
    build_id: String,
}

fn workflow_prefix() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".peri")
            .join("workflow")
            .join(WORKFLOW_NPM_VERSION),
    )
}

/// 校验固定 artifact 的 package identity、版本和入口契约。
fn validate_workflow_artifact(base: &Path) -> Option<PathBuf> {
    let package_dir = base
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    let metadata: WorkflowPackageMetadata =
        serde_json::from_slice(&std::fs::read(package_dir.join("package.json")).ok()?).ok()?;
    if metadata.name != WORKFLOW_PACKAGE_NAME
        || metadata.version != WORKFLOW_NPM_VERSION
        || metadata.main != WORKFLOW_ENTRY
        || metadata.protocol_version != WORKFLOW_PROTOCOL_VERSION
        || metadata.build_id != WORKFLOW_BUILD_ID
    {
        return None;
    }
    let entry = package_dir.join(&metadata.main);
    let canonical_package = package_dir.canonicalize().ok()?;
    let canonical_entry = entry.canonicalize().ok()?;
    if !canonical_entry.starts_with(&canonical_package)
        || !canonical_entry.metadata().ok()?.is_file()
    {
        return None;
    }
    let entry_bytes = std::fs::read(&canonical_entry).ok()?;
    if entry_bytes.as_slice() != WORKFLOW_ARTIFACT_BYTES {
        return None;
    }
    Some(entry)
}

pub(super) fn workflow_local_dist_in(base: &Path) -> Option<String> {
    validate_workflow_artifact(base).map(|path| path.to_string_lossy().into_owned())
}

fn workflow_local_dist() -> Option<String> {
    workflow_prefix().and_then(|prefix| workflow_local_dist_in(&prefix))
}

fn npx_fallback_allowed() -> bool {
    cfg!(test) || std::env::var_os(NPX_FALLBACK_ENV).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// 生产默认 fail closed；仅测试或显式 opt-in 时允许固定版本 npx fallback。
fn workflow_cmd() -> Result<WorkflowCommand, WorkflowError> {
    if let Some(dist) = workflow_local_dist() {
        return Ok(WorkflowCommand {
            program: "node".into(),
            args: vec![dist],
        });
    }
    if npx_fallback_allowed() {
        return Ok(WorkflowCommand {
            program: "npx".into(),
            args: vec![
                "-y".into(),
                format!("{WORKFLOW_PACKAGE_NAME}@{WORKFLOW_NPM_VERSION}"),
            ],
        });
    }
    Err(WorkflowError::SpawnFailed(format!(
        "validated workflow artifact {WORKFLOW_NPM_VERSION} is unavailable; set {NPX_FALLBACK_ENV}=1 to allow the network fallback"
    )))
}

async fn stop_install_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn run_install_with_timeout(child: &mut Child, timeout: Duration) -> std::io::Result<bool> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.map(|status| status.success()),
        Err(_) => {
            stop_install_child(child).await;
            Ok(false)
        }
    }
}

async fn publish_embedded_workflow_artifact() -> Result<(), WorkflowError> {
    if workflow_local_dist().is_some() {
        return Ok(());
    }
    let prefix = workflow_prefix().ok_or_else(|| {
        WorkflowError::SpawnFailed("HOME is unavailable for workflow artifact lookup".into())
    })?;
    let _guard = INSTALL_LOCK.lock().await;
    if workflow_local_dist().is_some() {
        return Ok(());
    }

    let parent = prefix.parent().ok_or_else(|| {
        WorkflowError::SpawnFailed("workflow artifact prefix has no parent".into())
    })?;
    tokio::fs::create_dir_all(parent).await?;
    if prefix.exists() {
        tokio::fs::remove_dir_all(&prefix).await?;
    }
    let staging = parent.join(format!(
        ".{WORKFLOW_NPM_VERSION}.staging-{}",
        uuid::Uuid::now_v7()
    ));
    let package = staging
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    tokio::fs::create_dir_all(package.join("dist")).await?;
    tokio::fs::write(
        package.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": WORKFLOW_PACKAGE_NAME,
            "version": WORKFLOW_NPM_VERSION,
            "main": WORKFLOW_ENTRY,
            "periProtocolVersion": WORKFLOW_PROTOCOL_VERSION,
            "periBuildId": WORKFLOW_BUILD_ID,
        }))?,
    )
    .await?;
    tokio::fs::write(package.join(WORKFLOW_ENTRY), WORKFLOW_ARTIFACT_BYTES).await?;

    if validate_workflow_artifact(&staging).is_none() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(
            "embedded workflow artifact failed validation".into(),
        ));
    }
    match tokio::fs::rename(&staging, &prefix).await {
        Ok(()) => {}
        Err(error) if validate_workflow_artifact(&prefix).is_some() => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            debug!(target: "workflow", error_kind = ?error.kind(), "another process published the workflow artifact");
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    }
    Ok(())
}

/// 安装到同文件系统 staging，完整校验后通过 rename 发布。
async fn ensure_workflow_install() -> Result<(), WorkflowError> {
    if workflow_local_dist().is_some() {
        return Ok(());
    }
    let prefix = workflow_prefix().ok_or_else(|| {
        WorkflowError::SpawnFailed("HOME is unavailable for workflow artifact lookup".into())
    })?;
    let _guard = INSTALL_LOCK.lock().await;
    if workflow_local_dist().is_some() {
        return Ok(());
    }

    let parent = prefix.parent().ok_or_else(|| {
        WorkflowError::SpawnFailed("workflow artifact prefix has no parent".into())
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let staging = parent.join(format!(
        ".{WORKFLOW_NPM_VERSION}.staging-{}",
        uuid::Uuid::now_v7()
    ));
    tokio::fs::create_dir(&staging).await?;

    let package = format!("{WORKFLOW_PACKAGE_NAME}@{WORKFLOW_NPM_VERSION}");
    let mut child = match Command::new("npm")
        .args(["install", "--prefix"])
        .arg(&staging)
        .arg(&package)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    };

    let installed = run_install_with_timeout(&mut child, INSTALL_TIMEOUT).await?;
    if !installed || validate_workflow_artifact(&staging).is_none() {
        stop_install_child(&mut child).await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(
            "workflow artifact installation failed validation".into(),
        ));
    }

    match tokio::fs::rename(&staging, &prefix).await {
        Ok(()) => {}
        Err(error) if validate_workflow_artifact(&prefix).is_some() => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            debug!(target: "workflow", error_kind = ?error.kind(), "another installer published the workflow artifact");
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    }
    if validate_workflow_artifact(&prefix).is_none() {
        return Err(WorkflowError::SpawnFailed(
            "published workflow artifact failed validation".into(),
        ));
    }
    info!(target: "workflow", version = WORKFLOW_NPM_VERSION, "installed validated workflow artifact");
    Ok(())
}

pub(super) async fn prepare_workflow_command() -> Result<WorkflowCommand, WorkflowError> {
    // 3. Publish the bundled artifact first so development, tests, and releases use
    // the same hermetic runtime. Network resolution remains an explicit fallback.
    if workflow_local_dist().is_none() {
        if let Err(error) = publish_embedded_workflow_artifact().await {
            if npx_fallback_allowed() {
                warn!(target: "workflow", error_kind = %error, "embedded workflow artifact unavailable; trying explicit network fallback");
                if let Err(install_error) = ensure_workflow_install().await {
                    warn!(target: "workflow", error_kind = %install_error, "workflow artifact install unavailable; using explicit npx fallback");
                }
            } else {
                return Err(error);
            }
        }
    }
    workflow_cmd()
}
