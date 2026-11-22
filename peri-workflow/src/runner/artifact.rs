//! 固定 Workflow artifact 的发行包发现、内容身份校验、缓存发布与命令准备。
//!
//! 红线（ARC-WORKFLOW-DIST-001）：Rust 侧零 JS 嵌入。本模块只持有入口内容摘要
//! 常量与身份契约，JS 产物始终以文件系统路径消费：随包 sidecar
//! （`<可执行文件目录>/peri-resources/workflow/<内容身份>/`）、本机缓存
//! （`~/.peri/workflow/<version>/<内容身份>/`）或显式注入的部署目录。
//! 运行时不访问 npm/registry：缺少已校验来源即 fail closed。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::error::WorkflowError;

/// 本地随包 Workflow engine 版本（与仓库 `npm-packages/@peri-workflow/package.json` 一致）。
///
/// 版本只用于本机缓存路径与包身份校验；运行时只消费随包 sidecar，不访问 npm。
const WORKFLOW_NPM_VERSION: &str = "0.3.0";
const WORKFLOW_PACKAGE_NAME: &str = "@peri-code/workflow";
const WORKFLOW_ENTRY: &str = "dist/peri-workflow.js";
pub(super) const WORKFLOW_PROTOCOL_VERSION: u32 = 3;
pub(super) const WORKFLOW_BUILD_ID: &str = "@peri-code/workflow@0.3.0";

/// 随包 pin 的入口内容摘要（sha256 十六进制小写），不是 JS 内容本身。
///
/// 与本地构建产物（`bun run build` 的构建产出，不入库）一致：bundle 变化时必须重新
/// 发布并同步本常量；仓库内不保存该产物，任何代码/测试都不读取工作树 dist。
const WORKFLOW_ENTRY_SHA256: &str =
    "ae03ca29c07dae162e4f15280f1b34b874480b02ca2251e3f3c687e3534ecbd1";

/// 随包 sidecar 目录（相对可执行文件所在目录）。
const SIDECAR_RELATIVE_DIR: &str = "peri-resources/workflow";
/// 显式注入的包目录（部署或测试）：包含 `package.json` 与 `dist/` 的包目录。
const ARTIFACT_DIR_ENV: &str = "PERI_WORKFLOW_ARTIFACT_DIR";

/// 串行化本机缓存的发布（避免并发 workflow 同时写入同一内容身份目录）。
static CACHE_PUBLISH_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
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
    content_id(WORKFLOW_ENTRY_SHA256)
}

/// 摘要对应的内容身份；malformed pin 原样返回，后续摘要校验会 fail closed。
fn content_id(sha256: &str) -> &str {
    sha256.get(..12).unwrap_or(sha256)
}

#[cfg(test)]
fn build_id_for(version: &str) -> String {
    format!("{WORKFLOW_PACKAGE_NAME}@{version}")
}

/// 单个来源期望的包身份（版本、构建标识与入口摘要）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PackageIdentity<'a> {
    version: &'a str,
    build_id: &'a str,
    sha256: &'a str,
}

impl<'a> PackageIdentity<'a> {
    /// 随包来源：版本与构建标识取编译期常量，摘要可注入（测试夹具）。
    fn bundled(sha256: &'a str) -> Self {
        Self {
            version: WORKFLOW_NPM_VERSION,
            build_id: WORKFLOW_BUILD_ID,
            sha256,
        }
    }
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

/// 期望的入口内容摘要（可注入）。
///
/// 生产恒为编译期 [`WORKFLOW_ENTRY_SHA256`]；测试用**合成夹具**时注入夹具自身的
/// 摘要，从而在不读取任何仓库文件的前提下覆盖「接受合法包」路径（仓库不承载 dist）。
#[derive(Debug, Clone)]
pub(crate) struct EntryPin(pub(crate) String);

impl Default for EntryPin {
    fn default() -> Self {
        Self(WORKFLOW_ENTRY_SHA256.to_string())
    }
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
    /// 期望的入口摘要（默认 = 生产 pin；测试可注入合成夹具的摘要）。
    pub(crate) entry_pin: EntryPin,
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
            entry_pin: EntryPin::default(),
        }
    }
}

/// 按「显式注入 → 随包缓存 → 随包 sidecar」顺序解析并校验入口。
///
/// 三者都校验随包 pin；显式注入无效时继续尝试其余来源。
pub(crate) fn resolve_artifact(sources: &ArtifactSources) -> Option<ResolvedArtifact> {
    let expected = sources.entry_pin.0.as_str();
    if let Some(dir) = sources.override_dir.as_deref() {
        if let Some(entry) = validate_workflow_package(dir, PackageIdentity::bundled(expected)) {
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
        let dir = prefix.join(content_id(expected));
        if let Some(entry) = validate_workflow_package(&dir, PackageIdentity::bundled(expected)) {
            return Some(ResolvedArtifact {
                package_dir: dir,
                entry,
                origin: ArtifactOrigin::Cache,
            });
        }
    }
    for dir in &sources.sidecar_dirs {
        if let Some(entry) = validate_workflow_package(dir, PackageIdentity::bundled(expected)) {
            return Some(ResolvedArtifact {
                package_dir: dir.clone(),
                entry,
                origin: ArtifactOrigin::Sidecar,
            });
        }
    }
    None
}

/// 校验包目录的身份、入口契约与入口内容摘要。
///
/// 返回入口路径；身份、越界路径或摘要任一不符返回 `None`（fail closed）。
fn validate_workflow_package(package_dir: &Path, expected: PackageIdentity<'_>) -> Option<PathBuf> {
    let metadata: WorkflowPackageMetadata =
        serde_json::from_slice(&std::fs::read(package_dir.join("package.json")).ok()?).ok()?;
    if metadata.name != WORKFLOW_PACKAGE_NAME
        || metadata.version != expected.version
        || metadata.main != WORKFLOW_ENTRY
        || metadata.protocol_version != WORKFLOW_PROTOCOL_VERSION
        || metadata.build_id != expected.build_id
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
    if !digest_matches(&std::fs::read(&canonical_entry).ok()?, expected.sha256) {
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
        .or_else(dirs::home_dir)?;
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
/// 没有任何已校验来源时 fail closed，不访问 npm/registry。
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
                    match publish_to_cache_in(
                        prefix,
                        &resolved.package_dir,
                        &resolved.entry,
                        PackageIdentity::bundled(sources.entry_pin.0.as_str()),
                    )
                    .await
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
    Err(WorkflowError::SpawnFailed(format!(
        "validated workflow artifact {WORKFLOW_NPM_VERSION} is unavailable: no sidecar at `{SIDECAR_RELATIVE_DIR}/{id}` next to the peri executable and no valid local cache; reinstall peri with the bundled resources or set {ARTIFACT_DIR_ENV} to a validated package directory",
        id = artifact_id()
    )))
}

/// 把已校验的 artifact 发布到缓存前缀（内容身份目录），不清理同版本下的其他内容
/// 或旧版本目录。
pub(crate) async fn publish_to_cache_in(
    prefix: &Path,
    package_dir: &Path,
    entry: &Path,
    expected: PackageIdentity<'_>,
) -> Result<PathBuf, WorkflowError> {
    let target = prefix.join(content_id(expected.sha256));
    if let Some(entry) = validate_workflow_package(&target, expected) {
        return Ok(entry);
    }

    let _guard = CACHE_PUBLISH_LOCK.lock().await;
    if let Some(entry) = validate_workflow_package(&target, expected) {
        return Ok(entry);
    }

    tokio::fs::create_dir_all(prefix).await?;
    let staging = prefix.join(format!(".staging-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir(&staging).await?;
    if let Err(error) = write_staged_package(package_dir, entry, &staging).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error);
    }
    if validate_workflow_package(&staging, expected).is_none() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(WorkflowError::SpawnFailed(
            "staged workflow artifact failed validation".into(),
        ));
    }

    match tokio::fs::rename(&staging, &target).await {
        Ok(()) => {}
        Err(error) if validate_workflow_package(&target, expected).is_some() => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            debug!(target: "workflow", error_kind = ?error.kind(), "another publisher created the workflow artifact cache entry");
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(WorkflowError::Io(error));
        }
    }
    validate_workflow_package(&target, expected).ok_or_else(|| {
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

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
