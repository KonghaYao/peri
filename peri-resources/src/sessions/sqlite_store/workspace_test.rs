use super::super::*;
use peri_acp_types::workspace::*;
#[cfg(unix)]
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    directory
}

async fn store() -> (SqliteThreadStore, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    (store, directory)
}

async fn bound(store: &SqliteThreadStore, cwd: &Path) -> (ThreadId, ResolvedWorkspace) {
    let workspace = store.resolve_workspace(cwd).await.unwrap();
    let id = store
        .create_bound_thread(
            ThreadMeta::new_at(cwd.to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    (id, workspace)
}

#[path = "workspace_discovery_test.rs"]
mod discovery;

#[path = "workspace_binding_test.rs"]
mod binding;

#[path = "workspace_execution_test.rs"]
mod execution;

#[path = "workspace_probe_test.rs"]
mod probe;
