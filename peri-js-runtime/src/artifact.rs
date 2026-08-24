use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use fs2::FileExt;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::process::Command;
use tracing::debug;

use crate::{JsProcessSpec, JsRuntimeError, Result};

pub(crate) const PACKAGE_NAME: &str = "@peri-code/ptc";
pub(crate) const PACKAGE_VERSION: &str = "0.2.3";
pub(crate) const PROTOCOL_VERSION: u64 = 1;
pub(crate) const BUILD_ID: &str = "@peri-code/ptc@0.2.3";
const ENTRY: &str = "dist/peri-ptc.js";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(90);
const FALLBACK_ENV: &str = "PERI_PTC_ALLOW_NPX_FALLBACK";
const PUBLIC_REGISTRY: &str = "https://registry.npmjs.org/";
const MAX_INSTALL_STDERR_BYTES: usize = 8 * 1024;

#[derive(Deserialize)]
struct PackageMetadata {
    name: String,
    version: String,
    main: String,
    bin: PackageBin,
    #[serde(rename = "periProtocolVersion")]
    protocol_version: u64,
    #[serde(rename = "periBuildId")]
    build_id: String,
}

#[derive(Deserialize)]
struct PackageBin {
    #[serde(rename = "peri-ptc")]
    entry: String,
}

pub(crate) struct PtcLaunch {
    pub(crate) spec: JsProcessSpec,
    pub(crate) local_cache: bool,
    _guard: Option<TempDir>,
}

#[async_trait::async_trait]
pub(crate) trait PtcArtifactProvider: Send + Sync {
    async fn launch(&self, node: &str) -> Result<PtcLaunch>;
    async fn invalidate(&self) -> Result<()>;
}

#[async_trait::async_trait]
pub(crate) trait Installer: Send + Sync {
    async fn install(&self, staging: &Path) -> std::io::Result<bool>;
}

struct NpmInstaller;

#[async_trait::async_trait]
impl Installer for NpmInstaller {
    async fn install(&self, staging: &Path) -> std::io::Result<bool> {
        let home = staging.join(".npm-home");
        let cache = staging.join(".npm-cache");
        tokio::fs::create_dir(&home).await?;
        tokio::fs::create_dir(&cache).await?;
        let child = match npm_command(staging, &home, &cache)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                debug!(error_kind = ?error.kind(), "PTC npm install failed to spawn");
                return Err(error);
            }
        };
        match tokio::time::timeout(INSTALL_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = bounded_stderr_tail(&output.stderr);
                    debug!(
                        status = ?output.status.code(),
                        stderr = %stderr,
                        "PTC npm install exited unsuccessfully"
                    );
                }
                Ok(output.status.success())
            }
            Ok(Err(error)) => {
                debug!(error_kind = ?error.kind(), "PTC npm install wait failed");
                Err(error)
            }
            Err(_) => {
                debug!(
                    timeout_seconds = INSTALL_TIMEOUT.as_secs(),
                    "PTC npm install timed out"
                );
                Ok(false)
            }
        }
    }
}

fn bounded_stderr_tail(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(MAX_INSTALL_STDERR_BYTES);
    String::from_utf8_lossy(&stderr[start..]).into_owned()
}

pub(crate) struct NpmArtifactProvider;

impl NpmArtifactProvider {
    pub(crate) fn new() -> Self {
        Self
    }

    fn home(&self) -> Result<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(JsRuntimeError::ArtifactUnavailable)
    }
}

#[async_trait::async_trait]
impl PtcArtifactProvider for NpmArtifactProvider {
    async fn launch(&self, node: &str) -> Result<PtcLaunch> {
        launch_in(node, &self.home()?, &NpmInstaller, fallback_enabled()).await
    }

    async fn invalidate(&self) -> Result<()> {
        quarantine(&self.home()?).await
    }
}

pub(crate) async fn launch_in(
    node: &str,
    home: &Path,
    installer: &dyn Installer,
    allow_fallback: bool,
) -> Result<PtcLaunch> {
    match ensure_install(home, installer).await {
        Ok(entry) => Ok(PtcLaunch {
            spec: JsProcessSpec::new(node, vec![entry.to_string_lossy().into_owned()])
                .without_inherited_environment()
                .with_environment(runtime_environment()),
            _guard: None,
            local_cache: true,
        }),
        Err(_) if allow_fallback => npx_launch(node),
        Err(_) => Err(JsRuntimeError::ArtifactUnavailable),
    }
}

fn prefix(home: &Path) -> PathBuf {
    home.join(".peri").join("ptc").join(PACKAGE_VERSION)
}

fn validate_install(base: &Path) -> Option<PathBuf> {
    let package = base.join("node_modules").join("@peri-code").join("ptc");
    let metadata: PackageMetadata =
        serde_json::from_slice(&std::fs::read(package.join("package.json")).ok()?).ok()?;
    if metadata.name != PACKAGE_NAME
        || metadata.version != PACKAGE_VERSION
        || metadata.main != "dist/index.js"
        || metadata.bin.entry != ENTRY
        || metadata.protocol_version != PROTOCOL_VERSION
        || metadata.build_id != BUILD_ID
    {
        return None;
    }
    let canonical_package = package.canonicalize().ok()?;
    let canonical_entry = package.join(&metadata.bin.entry).canonicalize().ok()?;
    let file = canonical_entry.metadata().ok()?;
    if !canonical_entry.starts_with(canonical_package) || !file.is_file() || file.len() == 0 {
        return None;
    }
    Some(canonical_entry)
}

struct InstallLock(File);

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

async fn acquire_lock(parent: &Path) -> Result<InstallLock> {
    tokio::fs::create_dir_all(parent).await?;
    let path = parent.join(format!(".{PACKAGE_VERSION}.lock"));
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock_exclusive()?;
        Ok::<_, std::io::Error>(InstallLock(file))
    })
    .await
    .map_err(|_| JsRuntimeError::ArtifactUnavailable)?
    .map_err(Into::into)
}

pub(crate) async fn ensure_install(home: &Path, installer: &dyn Installer) -> Result<PathBuf> {
    let target = prefix(home);
    if let Some(entry) = validate_install(&target) {
        return Ok(entry);
    }
    let parent = target.parent().ok_or(JsRuntimeError::ArtifactUnavailable)?;
    let _lock = acquire_lock(parent).await?;
    if let Some(entry) = validate_install(&target) {
        return Ok(entry);
    }
    if target.exists() {
        quarantine_locked(&target).await?;
    }
    let staging = parent.join(format!(
        ".{PACKAGE_VERSION}.staging-{}-{}",
        std::process::id(),
        unique_id()
    ));
    tokio::fs::create_dir(&staging).await?;
    let result = async {
        if !installer.install(&staging).await? || validate_install(&staging).is_none() {
            return Err(JsRuntimeError::ArtifactUnavailable);
        }
        match tokio::fs::rename(&staging, &target).await {
            Ok(()) => {}
            Err(_) if validate_install(&target).is_some() => {}
            Err(error) => return Err(error.into()),
        }
        validate_install(&target).ok_or(JsRuntimeError::ArtifactUnavailable)
    }
    .await;
    if staging.exists() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    result
}

pub(crate) async fn quarantine(home: &Path) -> Result<()> {
    let target = prefix(home);
    let parent = target.parent().ok_or(JsRuntimeError::ArtifactUnavailable)?;
    let _lock = acquire_lock(parent).await?;
    if target.exists() {
        quarantine_locked(&target).await?;
    }
    Ok(())
}

async fn quarantine_locked(target: &Path) -> Result<()> {
    let parent = target.parent().ok_or(JsRuntimeError::ArtifactUnavailable)?;
    let quarantine = parent.join(format!(
        ".{PACKAGE_VERSION}.quarantine-{}-{}",
        std::process::id(),
        unique_id()
    ));
    tokio::fs::rename(target, &quarantine).await?;
    tokio::fs::remove_dir_all(quarantine).await?;
    Ok(())
}

fn npm_command(staging: &Path, home: &Path, cache: &Path) -> Command {
    let mut command = Command::new(npm_program());
    command
        .args(install_args())
        .arg(staging)
        .arg(format!("{PACKAGE_NAME}@{PACKAGE_VERSION}"))
        .env_clear()
        .envs(path_environment())
        .env("HOME", home)
        .env("npm_config_cache", cache)
        .env("npm_config_registry", PUBLIC_REGISTRY);
    command
}

fn install_args() -> [&'static str; 6] {
    [
        "install",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--no-update-notifier",
        "--prefix",
    ]
}

fn npx_launch(node: &str) -> Result<PtcLaunch> {
    let guard = tempfile::Builder::new().prefix("peri-ptc-npx").tempdir()?;
    let home = guard.path().join("home");
    let cache = guard.path().join("npm-cache");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&cache)?;
    let mut environment = path_environment();
    environment.extend([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "npm_config_cache".into(),
            cache.to_string_lossy().into_owned(),
        ),
        ("npm_config_registry".into(), PUBLIC_REGISTRY.into()),
    ]);
    let spec = JsProcessSpec::new(npx_program(node), fallback_args())
        .with_cwd(guard.path().to_string_lossy())
        .without_inherited_environment()
        .with_environment(environment);
    Ok(PtcLaunch {
        spec,
        _guard: Some(guard),
        local_cache: false,
    })
}

fn npm_program() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

fn npx_program(node: &str) -> String {
    let node = Path::new(node);
    if node.components().count() > 1 {
        if let Some(parent) = node.parent() {
            return parent
                .join(if cfg!(windows) { "npx.cmd" } else { "npx" })
                .to_string_lossy()
                .into_owned();
        }
    }
    if cfg!(windows) { "npx.cmd" } else { "npx" }.into()
}

#[cfg(windows)]
fn runtime_environment() -> Vec<(String, String)> {
    let mut environment = path_environment();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(name) {
            environment.push((name.into(), value));
        }
    }
    environment
}

#[cfg(not(windows))]
fn runtime_environment() -> Vec<(String, String)> {
    path_environment()
}

fn path_environment() -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_else(|_| default_path().into());
    vec![("PATH".into(), path)]
}

#[cfg(windows)]
fn default_path() -> &'static str {
    r"C:\Windows\System32;C:\Windows"
}

#[cfg(not(windows))]
fn default_path() -> &'static str {
    "/usr/local/bin:/usr/bin:/bin"
}

fn fallback_enabled() -> bool {
    std::env::var_os(FALLBACK_ENV).as_deref() == Some(OsStr::new("1"))
}

fn fallback_args() -> Vec<String> {
    vec![
        "-y".into(),
        "--ignore-scripts".into(),
        "--no-audit".into(),
        "--no-fund".into(),
        format!("{PACKAGE_NAME}@{PACKAGE_VERSION}"),
    ]
}

fn unique_id() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    struct FixtureInstaller {
        calls: AtomicUsize,
        metadata: serde_json::Value,
        entry: &'static [u8],
    }

    #[async_trait::async_trait]
    impl Installer for FixtureInstaller {
        async fn install(&self, staging: &Path) -> std::io::Result<bool> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let package = staging.join("node_modules/@peri-code/ptc");
            tokio::fs::create_dir_all(package.join("dist")).await?;
            tokio::fs::write(package.join("package.json"), self.metadata.to_string()).await?;
            tokio::fs::write(package.join(ENTRY), self.entry).await?;
            tokio::fs::write(package.join("dist/index.js"), b"export {};").await?;
            Ok(true)
        }
    }

    fn metadata(entry: &str) -> serde_json::Value {
        serde_json::json!({
            "name": PACKAGE_NAME,
            "version": PACKAGE_VERSION,
            "main": "dist/index.js",
            "bin": { "peri-ptc": entry },
            "periProtocolVersion": PROTOCOL_VERSION,
            "periBuildId": BUILD_ID
        })
    }

    fn fixture() -> FixtureInstaller {
        FixtureInstaller {
            calls: AtomicUsize::new(0),
            metadata: metadata(ENTRY),
            entry: b"#!/usr/bin/env node\n",
        }
    }

    #[tokio::test]
    async fn concurrent_install_has_one_winner_and_reuses_target() {
        let home = tempfile::tempdir().unwrap();
        let installer = Arc::new(fixture());
        let (left, right) = tokio::join!(
            ensure_install(home.path(), installer.as_ref()),
            ensure_install(home.path(), installer.as_ref())
        );
        assert_eq!(left.unwrap(), right.unwrap());
        assert_eq!(installer.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn corrupt_target_is_quarantined_then_reinstalled() {
        let home = tempfile::tempdir().unwrap();
        let target = prefix(home.path());
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::write(target.join("in-use-marker"), b"keep")
            .await
            .unwrap();
        ensure_install(home.path(), &fixture()).await.unwrap();
        let entries = std::fs::read_dir(target.parent().unwrap()).unwrap();
        assert!(entries
            .filter_map(|entry| entry.ok())
            .all(|entry| !entry.file_name().to_string_lossy().contains("quarantine")));
        assert!(validate_install(&target).is_some());
    }

    #[test]
    fn runtime_environment_is_secret_free() {
        let environment = runtime_environment()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(environment.contains_key("PATH"));
        for secret in ["NODE_OPTIONS", "NPM_TOKEN", "AWS_SECRET_ACCESS_KEY"] {
            assert!(!environment.contains_key(secret));
        }
        #[cfg(windows)]
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if std::env::var_os(name).is_some() {
                assert!(environment.contains_key(name));
            }
        }
    }

    #[test]
    fn npm_contract_is_exact_and_secret_free() {
        assert_eq!(
            install_args(),
            [
                "install",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--no-update-notifier",
                "--prefix"
            ]
        );
        assert_eq!(
            fallback_args(),
            [
                "-y",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "@peri-code/ptc@0.2.3"
            ]
        );
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("cache");
        let controlled_home = home.path().join("home");
        let command = npm_command(home.path(), &controlled_home, &cache);
        let command = command.as_std();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "install",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--no-update-notifier",
                "--prefix",
                home.path().to_string_lossy().as_ref(),
                "@peri-code/ptc@0.2.3",
            ]
        );
        let environment = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment.keys().map(String::as_str).collect::<Vec<_>>(),
            ["HOME", "PATH", "npm_config_cache", "npm_config_registry"]
        );
        assert_eq!(
            environment["HOME"].as_deref(),
            Some(controlled_home.to_string_lossy().as_ref())
        );
        assert_eq!(
            environment["npm_config_cache"].as_deref(),
            Some(cache.to_string_lossy().as_ref())
        );
        assert_eq!(
            environment["npm_config_registry"].as_deref(),
            Some(PUBLIC_REGISTRY)
        );
        for secret in ["NODE_OPTIONS", "NPM_TOKEN", "AWS_SECRET_ACCESS_KEY"] {
            assert!(!environment.contains_key(secret));
        }
    }

    #[tokio::test]
    async fn rejects_wrong_identity_and_path_escape() {
        for package in [
            {
                let mut value = metadata(ENTRY);
                value["version"] = "9.9.9".into();
                value
            },
            metadata("../../../../escaped.js"),
        ] {
            let home = tempfile::tempdir().unwrap();
            let installer = FixtureInstaller {
                calls: AtomicUsize::new(0),
                metadata: package,
                entry: b"x",
            };
            assert!(ensure_install(home.path(), &installer).await.is_err());
        }
    }
}
