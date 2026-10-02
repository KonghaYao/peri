//! 会话资源门面行为测试（本机 SQLite）。
//!
//! 断言以可观察结果为准：写入准入是否统一、效果结清是否只认确定性、创建/撤销/认领的
//! 后置条件、删除与未决证据在级联之后是否仍可判定。构造「数据已保存、runtime 未准入」
//! 这类状态时直接经数据端口写入——那正是远程保存或上次进程留下的状态。

use super::*;
use crate::sessions::local_port::SessionFacts;
use crate::sessions::sqlite_store::{commit_failure, write_failure};
use crate::SessionStoreShutdownOwner;
use peri_acp_types::session_resources::{
    FrozenState, NewSessionDraft, NewSessionMeta, SessionInitialization, SessionResourceResult,
    SessionResources, SessionStoreShutdownPort,
};
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

struct Fixture {
    /// 具体实例：`Arc` 让业务侧与部署 owner 指向同一份事实（生产装配同形）。
    facade: Arc<SessionResourcesImpl>,
    repo: TempDir,
    _db: TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let repo = repository();
        let db = tempfile::tempdir().unwrap();
        let facade = Arc::new(
            SessionResourcesImpl::open(db.path().join("threads.db"))
                .await
                .unwrap(),
        );
        Self {
            facade,
            repo,
            _db: db,
        }
    }

    /// 部署 owner 的关闭路径：装配点交出的唯一关闭权（门面自身不再对业务暴露关闭）。
    async fn shutdown(&self) -> SessionResourceResult<()> {
        SessionStoreShutdownOwner::take(Arc::clone(&self.facade))
            .shutdown()
            .await
    }

    async fn workspace(&self) -> ResolvedWorkspace {
        self.facade
            .resolve_workspace(self.repo.path())
            .await
            .unwrap()
    }

    fn binding(workspace: &ResolvedWorkspace) -> SessionBinding {
        SessionBinding::from_workspace(workspace)
    }

    fn session(&self, id: &str, workspace: &ResolvedWorkspace, frozen: &str) -> NewSession {
        NewSession {
            thread_id: id.to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("session {id}")),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Self::binding(workspace),
            frozen: FrozenSnapshotBytes::new(frozen.to_owned()),
        }
    }

    /// 建一个完整会话并返回 owner。
    async fn create(&self, id: &str) -> Arc<dyn SessionExecutionLease> {
        let workspace = self.workspace().await;
        let input = self.session(id, &workspace, &format!(r#"{{"v":1,"id":"{id}"}}"#));
        self.facade.create_session(&input).await.unwrap()
    }

    /// 未发布创建（J2 第一阶段）：草稿 + lease，frozen 尚未提交。
    async fn begin(&self, id: &str) -> Arc<dyn SessionInitialization> {
        let workspace = self.workspace().await;
        let draft = NewSessionDraft {
            thread_id: id.to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("session {id}")),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Self::binding(&workspace),
        };
        self.facade.begin_initialization(&draft).await.unwrap()
    }

    /// 直接经数据面落一份「数据已保存、runtime 未准入」的会话。
    async fn save_without_admission(&self, id: &str, workspace: &ResolvedWorkspace) {
        let input = self.session(id, workspace, &format!(r#"{{"v":1,"id":"{id}"}}"#));
        self.facade
            .gate
            .data()
            .save_new_session(&input)
            .await
            .unwrap();
    }

    async fn count_threads(&self, id: &str) -> i64 {
        self.count("SELECT COUNT(*) FROM threads WHERE id = ?1", id)
            .await
    }

    async fn count_messages(&self, id: &str) -> i64 {
        self.count("SELECT COUNT(*) FROM messages WHERE thread_id = ?1", id)
            .await
    }

    async fn count_bindings(&self, id: &str) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM session_bindings WHERE thread_id = ?1",
            id,
        )
        .await
    }

    async fn count(&self, sql: &'static str, id: &str) -> i64 {
        let row: (i64,) = sqlx::query_as(sql)
            .bind(id)
            .fetch_one(self.facade.local_pool())
            .await
            .unwrap();
        row.0
    }
}

fn payload(text: &str) -> PersistedPayload {
    PersistedPayload::Message(peri_acp_types::messages::BaseMessage::human(text))
}

fn error_kind(error: &SessionResourceError) -> &SessionResourceErrorKind {
    error.kind()
}

async fn facts_of(facade: &SessionResourcesImpl, id: &str) -> SessionFacts {
    facade.gate.session_facts(&id.to_owned()).await.unwrap()
}

impl Fixture {
    /// `threads.frozen_context` 原值（`None` = 从未提交/半写草稿）。
    async fn frozen_of(&self, id: &str) -> Option<String> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                .bind(id)
                .fetch_optional(self.facade.local_pool())
                .await
                .unwrap();
        row.and_then(|(frozen,)| frozen)
    }

    /// 同一库的第二个宿主句柄（另一条连接、另一套进程内登记）。
    async fn second_host(&self) -> Arc<SessionResourcesImpl> {
        Arc::new(
            SessionResourcesImpl::open(self._db.path().join("threads.db"))
                .await
                .unwrap(),
        )
    }

    async fn visible_ids(&self) -> Vec<String> {
        self.facade
            .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
                scope: peri_acp_types::workspace::ThreadScope::All,
                cursor: None,
                limit: 50,
            })
            .await
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.thread.id.clone())
            .collect()
    }
}

impl Fixture {
    /// 在 root 之下建一个 child（frozen 取自 root 的已保存快照）。
    async fn child(
        &self,
        child_id: &str,
        root: &str,
        root_lease: &Arc<dyn SessionExecutionLease>,
    ) -> ChildSnapshot {
        let snapshot = self.child_snapshot(child_id, root).await;
        self.facade.save_child(&snapshot, root_lease).await.unwrap();
        snapshot
    }

    /// 只构造 child 快照、不保存：用于在准入被占用时观察写入是否真的等门禁。
    async fn child_snapshot(&self, child_id: &str, root: &str) -> ChildSnapshot {
        let workspace = self.workspace().await;
        let root_frozen = self
            .facade
            .load_session_snapshot(&root.to_owned())
            .await
            .unwrap()
            .frozen;
        let FrozenState::Present(root_frozen) = root_frozen else {
            panic!("root frozen snapshot is missing");
        };
        ChildSnapshot {
            target: NewSession {
                thread_id: child_id.to_owned(),
                created_at: "2026-09-26T00:00:01Z".to_owned(),
                meta: NewSessionMeta {
                    title: Some(format!("child {child_id}")),
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: Some(root.to_owned()),
                    hidden: true,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: Fixture::binding(&workspace),
                frozen: root_frozen,
            },
            parent_id: root.to_owned(),
            root_id: root.to_owned(),
            inherited: peri_acp_types::store::InheritedContext {
                payloads: Vec::new(),
                flags: std::collections::HashMap::new(),
            },
        }
    }
}

#[path = "resources_admission_test.rs"]
mod admission;

#[path = "resources_initialization_test.rs"]
mod initialization;

#[path = "resources_child_test.rs"]
mod child;

#[path = "resources_lifecycle_test.rs"]
mod lifecycle;

#[path = "resources_fork_test.rs"]
mod fork;

#[path = "resources_composition_test.rs"]
mod composition;
