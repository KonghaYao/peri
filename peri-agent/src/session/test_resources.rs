//! 会话资源测试夹具：真实门面 + 临时 git 工作区 + 活跃执行所有权。
//!
//! 不造假的存储替身：写入门禁要求「本 root 有活 owner」，所以夹具真的建立一条会话并
//! 持有它的 lease——这正是生产路径的前置条件（只读、无主的会话在门面上本来就写不了）。
//! 需要构造真实写入失败时，调用 [`TestSession::release_lease`]：owner 消失后写入按
//! `LeaseRequired` 失败，而不是靠 mock 假装失败。

#[path = "test_resources/mock/mod.rs"]
pub(crate) mod mock;

use std::sync::Arc;

use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{SessionBinding, SessionExecutionLease, SESSION_BINDING_VERSION};
use peri_resources::sessions::SessionResourcesImpl;

/// 一条已创建、已取得执行所有权的会话（临时库 + 临时 git 工作区）。
pub(crate) struct TestSession {
    pub(crate) resources: Arc<dyn SessionResources>,
    pub(crate) thread_id: ThreadId,
    lease: Option<Arc<dyn SessionExecutionLease>>,
    _db: tempfile::TempDir,
    _repo: tempfile::TempDir,
}

impl TestSession {
    pub(crate) async fn open() -> Self {
        let repo = git_repository();
        let db = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(db.path().join("threads.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(repo.path()).await.unwrap();
        let thread_id = uuid::Uuid::now_v7().to_string();
        let session = NewSession {
            thread_id: thread_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: Some("test session".to_owned()),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding {
                schema_version: SESSION_BINDING_VERSION,
                revision: 1,
                project_id: workspace.project_id,
                workspace_id: workspace.workspace_id,
                cwd_relative_to_workspace: workspace.relative_cwd.clone(),
            },
            frozen: FrozenSnapshotBytes::new("{\"version\":1,\"test\":true}"),
        };
        let lease = resources.create_session(&session).await.unwrap();
        Self {
            resources,
            thread_id,
            lease: Some(lease),
            _db: db,
            _repo: repo,
        }
    }

    /// 同一条会话的另一个门面句柄（用于验证「写入真的落库」而不是只改了内存）。
    pub(crate) fn resources(&self) -> Arc<dyn SessionResources> {
        Arc::clone(&self.resources)
    }

    /// 丢弃执行所有权：此后本会话的写入按 `LeaseRequired` 真实失败。
    pub(crate) fn release_lease(&mut self) {
        self.lease = None;
    }
}

/// 临时 git 仓库（工作区发现需要真实仓库证据）。
pub(crate) fn git_repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
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
    ] {
        let output = std::process::Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", directory.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(directory.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git fixture failed");
    }
    directory
}
