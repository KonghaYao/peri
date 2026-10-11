//! 固定 Workflow artifact 的发行包发现、内容身份校验、缓存发布与命令准备。
//!
//! 红线（ARC-WORKFLOW-DIST-001）：Rust 侧零 JS 嵌入。本模块只持有入口内容摘要
//! 常量与身份契约，JS 产物始终以文件系统路径消费：随包 sidecar
//! （`<可执行文件目录>/peri-resources/workflow/<内容身份>/`）、本机缓存
//! （`~/.peri/workflow/<version>/<内容身份>/`）或显式注入的部署目录。
//! 网络获取仅显式 opt-in，且获取内容必须通过同一摘要校验后才可能被执行。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use peri_process::ProcessTree;
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use crate::error::WorkflowError;

/// 本地固定 Workflow engine 版本（与 npm 发布版本保持一致）。
const WORKFLOW_NPM_VERSION: &str = "0.2.0";
const WORKFLOW_PACKAGE_NAME: &str = "@peri-code/workflow";
const WORKFLOW_ENTRY: &str = "dist/peri-workflow.js";
/// 网络获取的超时（仅显式 opt-in 路径使用）。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(90);
pub(super) const WORKFLOW_PROTOCOL_VERSION: u32 = 1;
pub(super) const WORKFLOW_BUILD_ID: &str = "@peri-code/workflow@0.2.0";

/// pin 的入口内容摘要（sha256 十六进制小写），不是 JS 内容本身。
///
/// 与入库的 `npm-packages/@peri-workflow/dist/peri-workflow.js` 一致；dist 重建后
/// 必须同步更新，`repository_bundle_matches_pinned_digest` 测试守卫两者一致。
const WORKFLOW_ENTRY_SHA256: &str =
    "89b9b605954847267fa30107935e2454c029ecb75b64d27844367cc75b866c92";

/// 随包 sidecar 目录（相对可执行文件所在目录）。
const SIDECAR_RELATIVE_DIR: &str = "peri-resources/workflow";
/// 显式注入的包目录（部署或测试）：包含 `package.json` 与 `dist/` 的 npm 包目录。
const ARTIFACT_DIR_ENV: &str = "PERI_WORKFLOW_ARTIFACT_DIR";
/// 显式 opt-in 的网络获取开关（值必须恰为 `1`）。
///
/// 获取内容写入 staging 后仍须通过同一摘要校验；任一步失败都以 [`WorkflowError`]
/// 结束，不存在「未校验即执行」的网络路径。
const NETWORK_FETCH_ENV: &str = "PERI_WORKFLOW_ALLOW_NPX_FALLBACK";

/// 串行化本机缓存的发布（避免并发 workflow 同时写入同一内容身份目录）。
static INSTALL_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkflowCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
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

/// 内容身份：pin 摘要的前 12 位十六进制。
///
/// 发行包 sidecar 目录名与本机缓存内容目录共用该身份；同一版本号下旧内容因摘要
/// 不同而位于不同目录并校验失败，不会被混用。
pub(crate) fn artifact_id() -> &'static str {
    &WORKFLOW_ENTRY_SHA256[..12]
}

/// artifact 来源：决定是否在运行前发布到本机缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactOrigin {
    /// 显式注入（部署或测试）：直接使用，不写入本机缓存。
    Override,
    /// 本机缓存。
    Cache,
    /// 随包 sidecar：使用前 best-effort 发布到本机缓存以便复用。
    Sidecar,
}

/// 通过身份、路径与摘要校验的 artifact。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedArtifact {
    pub(crate) package_dir: PathBuf,
    pub(crate) entry: PathBuf,
    pub(crate) origin: ArtifactOrigin,
}

/// 解析输入：从进程环境派生，或由测试/部署显式构造。
#[derive(Debug, Default, Clone)]
pub(crate) struct ArtifactSources {
    /// 显式注入的包目录（`PERI_WORKFLOW_ARTIFACT_DIR`）。
    pub(crate) override_dir: Option<PathBuf>,
    /// 随包 sidecar 包目录候选（按可执行文件目录派生）。
    pub(crate) sidecar_dirs: Vec<PathBuf>,
    /// 本机缓存前缀（`~/.peri/workflow/<version>`）。
    pub(crate) cache_prefix: Option<PathBuf>,
}

impl ArtifactSources {
    /// 从进程环境派生：显式注入、随包 sidecar 候选、本机缓存前缀。
    pub(crate) fn from_process() -> Self {
        Self {
            override_dir: std::env::var_os(ARTIFACT_DIR_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            sidecar_dirs: executable_dirs()
                .into_iter()
                .map(|dir| dir.join(SIDECAR_RELATIVE_DIR).join(artifact_id()))
                .collect(),
            cache_prefix: workflow_prefix(),
        }
    }
}

/// 按「显式注入 → 本机缓存 → 随包 sidecar」顺序解析并校验入口。
///
/// 显式注入无效时继续尝试其余来源（三者校验口径一致，接受的必为同一内容）。
pub(crate) fn resolve_artifact(sources: &ArtifactSources) -> Option<ResolvedArtifact> {
    if let Some(dir) = sources.override_dir.as_deref() {
        if let Some(entry) = validate_workflow_package(dir) {
            return Some(ResolvedArtifact {
                package_dir: dir.to_path_buf(),
                entry,
                origin: ArtifactOrigin::Override,
            });
        }
        warn!(
            target: "workflow",
            path = %dir.display(),
            "explicitly configured workflow artifact failed validation; falling back to cache/ bundled sources"
        );
    }
    if let Some(prefix) = sources.cache_prefix.as_deref() {
        let dir = prefix.join(artifact_id());
        if let Some(entry) = validate_workflow_package(&dir) {
            return Some(ResolvedArtifact {
                package_dir: dir,
                entry,
                origin: ArtifactOrigin::Cache,
            });
        }
    }
    for dir in &sources.sidecar_dirs {
        if let Some(entry) = validate_workflow_package(dir) {
            return Some(ResolvedArtifact {
                package_dir: dir.clone(),
                entry,
                origin: ArtifactOrigin::Sidecar,
            });
        }
    }
    None
}

/// 校验 npm 包目录的身份、入口契约与入口内容摘要。
///
/// 返回入口路径；身份、越界路径或摘要任一不符返回 `None`（fail closed）。
fn validate_workflow_package(package_dir: &Path) -> Option<PathBuf> {
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
    if !digest_matches(
        &std::fs::read(&canonical_entry).ok()?,
        WORKFLOW_ENTRY_SHA256,
    ) {
        return None;
    }
    Some(entry)
}

/// 入口字节的 sha256 是否等于 pin 摘要（大小写不敏感）。
fn digest_matches(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes).eq_ignore_ascii_case(expected_hex)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn workflow_prefix() -> Option<PathBuf> {
    // Keep explicit HOME overrides portable (including isolated subprocess tests),
    // while supporting Windows profiles where HOME is normally absent.
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs_next::home_dir)?;
    Some(
        home.join(".peri")
            .join("workflow")
            .join(WORKFLOW_NPM_VERSION),
    )
}

/// 可执行文件所在目录候选：解析后的路径优先，其次原始路径（兼容 symlink 启动）。
fn executable_dirs() -> Vec<PathBuf> {
    let Ok(current) = std::env::current_exe() else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    if let Some(parent) = current
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        dirs.push(parent);
    }
    if let Some(parent) = current.parent() {
        let dir = parent.to_path_buf();
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

fn node_command(entry: PathBuf) -> WorkflowCommand {
    WorkflowCommand {
        program: "node".into(),
        args: vec![entry.to_string_lossy().into_owned()],
    }
}

/// 准备运行入口：解析已校验 artifact；随包 sidecar 先 best-effort 发布到本机缓存。
///
/// 没有任何已校验来源时 fail closed；仅在显式 opt-in 时尝试网络获取，且获取内容
/// 必须通过同一摘要校验。
pub(crate) async fn prepare_workflow_command() -> Result<WorkflowCommand, WorkflowError> {
    prepare_workflow_command_with(&ArtifactSources::from_process()).await
}

/// [`prepare_workflow_command`] 的可注入版本（解析输入显式给出，便于测试与诊断）。
pub(crate) async fn prepare_workflow_command_with(
    sources: &ArtifactSources,
) -> Result<WorkflowCommand, WorkflowError> {
    if let Some(resolved) = resolve_artifact(sources) {
        let entry = if resolved.origin == ArtifactOrigin::Sidecar {
            match sources.cache_prefix.as_deref() {
                Some(prefix) => {
                    match publish_to_cache_in(prefix, &resolved.package_dir, &resolved.entry).await
                    {
                        Ok(entry) => entry,
                        Err(error) => {
                            warn!(
                                target: "workflow",
                                %error,
                                "bundled workflow artifact could not be published to the local cache; using it in place"
                            );
                            resolved.entry
                        }
                    }
                }
                None => resolved.entry,
            }
        } else {
            resolved.entry
        };
        return Ok(node_command(entry));
    }
    if network_fetch_allowed() {
        return fetch_and_publish_from_registry().await.map(node_command);
    }
    Err(WorkflowError::SpawnFailed(format!(
        "validated workflow artifact {WORKFLOW_NPM_VERSION} is unavailable: no sidecar at `{SIDECAR_RELATIVE_DIR}/{id}` next to the peri executable and no valid local cache; reinstall peri with the bundled resources, set {ARTIFACT_DIR_ENV}, or set {NETWORK_FETCH_ENV}=1 to allow a validated registry fetch",
        id = artifact_id()
    )))
}

/// 把已校验的 artifact 发布到缓存前缀（内容身份目录），不清理同版本下的其他内容
/// 或旧版本目录。
pub(crate) async fn publish_to_cache_in(
    prefix: &Path,
    package_dir: &Path,
    entry: &Path,
) -> Result<PathBuf, WorkflowError> {
    let target = prefix.join(artifact_id());
    if let Some(entry) = validate_workflow_package(&target) {
        return Ok(entry);
    }

    let _guard = INSTALL_LOCK.lock().await;
    if let Some(entry) = validate_workflow_package(&target) {
        return Ok(entry);
    }

    tokio::fs::create_dir_all(prefix).await?;
    let staging = prefix.join(format!(".staging-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir(&staging).await?;
    if let Err(error) = write_staged_package(package_dir, entry, &staging).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error);
    }
    if validate_workflow_package(&staging).is_none() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(
            "staged workflow artifact failed validation".into(),
        ));
    }

    match tokio::fs::rename(&staging, &target).await {
        Ok(()) => {}
        Err(error) if validate_workflow_package(&target).is_some() => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            debug!(target: "workflow", error_kind = ?error.kind(), "another publisher created the workflow artifact cache entry");
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    }
    validate_workflow_package(&target).ok_or_else(|| {
        WorkflowError::SpawnFailed("published workflow artifact failed validation".into())
    })
}

/// 从已校验的包目录复制 `package.json` 与入口到 staging（内容再校验后才发布）。
async fn write_staged_package(
    package_dir: &Path,
    entry: &Path,
    staging: &Path,
) -> Result<(), WorkflowError> {
    tokio::fs::create_dir_all(staging.join("dist")).await?;
    tokio::fs::copy(
        package_dir.join("package.json"),
        staging.join("package.json"),
    )
    .await?;
    tokio::fs::copy(entry, staging.join(WORKFLOW_ENTRY)).await?;
    Ok(())
}

fn network_fetch_allowed() -> bool {
    network_fetch_allowed_from(std::env::var_os(NETWORK_FETCH_ENV).as_deref())
}

/// 网络获取的准入判定：值必须恰为 `1`；不因测试构建放开。
fn network_fetch_allowed_from(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

async fn stop_fetch_child(child: &mut Child, tree: &ProcessTree) {
    tree.terminate();
    let _ = child.kill().await;
    let _ = child.wait().await;
    tree.wait_for_exit().await;
}

async fn run_fetch_with_timeout(
    child: &mut Child,
    tree: &ProcessTree,
    timeout: Duration,
) -> std::io::Result<bool> {
    match peri_time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let status = status?;
            tree.wait_for_exit().await;
            Ok(status.success())
        }
        Err(_) => {
            stop_fetch_child(child, tree).await;
            Ok(false)
        }
    }
}

/// 显式 opt-in 的 registry 获取：安装到 staging → 同一摘要校验 → 发布到本机缓存。
///
/// 未通过校验的获取内容一律丢弃并以错误结束；不存在直接执行网络内容的路径。
async fn fetch_and_publish_from_registry() -> Result<PathBuf, WorkflowError> {
    let prefix = workflow_prefix().ok_or_else(|| {
        WorkflowError::SpawnFailed("HOME is unavailable for the workflow artifact cache".into())
    })?;
    let parent = prefix.parent().ok_or_else(|| {
        WorkflowError::SpawnFailed("workflow artifact cache prefix has no parent".into())
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let staging = parent.join(format!(
        ".{WORKFLOW_NPM_VERSION}.fetch-{}",
        uuid::Uuid::now_v7()
    ));
    tokio::fs::create_dir(&staging).await?;

    let package = format!("{WORKFLOW_PACKAGE_NAME}@{WORKFLOW_NPM_VERSION}");
    let mut command = Command::new("npm");
    command
        .args(["install", "--prefix"])
        .arg(&staging)
        .arg(&package)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let (mut child, tree) = match crate::process::spawn(command) {
        Ok(child) => child,
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    };

    let installed = run_fetch_with_timeout(&mut child, &tree, INSTALL_TIMEOUT).await?;
    let package_dir = staging
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    let entry = if installed {
        validate_workflow_package(&package_dir)
    } else {
        None
    };
    let Some(entry) = entry else {
        stop_fetch_child(&mut child, &tree).await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(format!(
            "workflow artifact fetched from the registry failed validation against pinned sha256 {WORKFLOW_ENTRY_SHA256}"
        )));
    };

    let published = publish_to_cache_in(&prefix, &package_dir, &entry).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let published = published?;
    info!(
        target: "workflow",
        version = WORKFLOW_NPM_VERSION,
        "fetched and validated workflow artifact from the registry"
    );
    Ok(published)
}

/// [测试专用] 入库 npm 包目录（`PERI_WORKFLOW_ARTIFACT_DIR` 的注入目标）。
#[cfg(test)]
pub(crate) fn repository_package_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("npm-packages")
        .join("@peri-workflow")
}

/// [测试专用] 入库 bundle 的路径（`npm-packages/@peri-workflow/dist`）。
#[cfg(test)]
pub(crate) fn repository_bundle_path() -> PathBuf {
    repository_package_dir().join(WORKFLOW_ENTRY)
}

/// [测试专用] 以入库 bundle 直接构造命令：测试真实执行随包产物，缺失即失败。
#[cfg(test)]
pub(crate) fn repository_workflow_command() -> WorkflowCommand {
    let entry = repository_bundle_path();
    assert!(
        entry.is_file(),
        "仓库必须包含已入库的 workflow bundle: {}",
        entry.display()
    );
    node_command(entry)
}

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
