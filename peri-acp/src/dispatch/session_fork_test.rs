use std::io;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::store::{CompactionLifecycle, MessageFlags, PersistedPayload, ThreadStore};
use peri_acp_types::thread::{ThreadId, ThreadListEntry, ThreadMeta};
use peri_controller::Controller;
use peri_resources::sessions::{FilesystemThreadStore, SqliteThreadStore};
use tracing_subscriber::fmt::MakeWriter;

use super::fork_session;

const COPY_CANARY: &str = "COPY_CANARY /private/secret-copy.db postgres://user:pass@host/copy";
const CLEANUP_CANARY: &str =
    "CLEANUP_CANARY /private/secret-cleanup.db postgres://user:pass@host/cleanup";

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CapturedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedWriter(self.0.clone())
    }
}

impl CapturedLogs {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

struct FailingForkStore {
    inner: FilesystemThreadStore,
    fail_delete: bool,
}

#[async_trait]
impl ThreadStore for FailingForkStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        self.inner.create_thread(meta).await
    }

    async fn append_messages(&self, _id: &ThreadId, _msgs: &[BaseMessage]) -> Result<()> {
        Err(anyhow!(COPY_CANARY))
    }

    async fn append_payloads(&self, _id: &ThreadId, _payloads: &[PersistedPayload]) -> Result<()> {
        Err(anyhow!(COPY_CANARY))
    }

    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        self.inner.load_messages(id).await
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        self.inner.load_meta(id).await
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        self.inner.update_meta(id, meta).await
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        self.inner.list_threads().await
    }

    async fn list_thread_entries(&self, cwd: &str) -> Result<Vec<ThreadListEntry>> {
        self.inner.list_thread_entries(cwd).await
    }

    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        if self.fail_delete {
            Err(anyhow!(CLEANUP_CANARY))
        } else {
            self.inner.delete_thread(id).await
        }
    }

    async fn load_context(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        self.inner.load_context(id).await
    }

    async fn list_child_threads(&self, id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        self.inner.list_child_threads(id).await
    }

    async fn list_session_threads(&self, id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        self.inner.list_session_threads(id).await
    }

    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()> {
        self.inner.update_thread_status(id, status).await
    }

    async fn invalidate_context_cache(&self, id: &ThreadId) -> Result<()> {
        self.inner.invalidate_context_cache(id).await
    }

    async fn delete_messages(&self, id: &ThreadId, message_ids: &[MessageId]) -> Result<()> {
        self.inner.delete_messages(id, message_ids).await
    }

    async fn update_message_flags(&self, id: &MessageId, flags: &MessageFlags) -> Result<()> {
        self.inner.update_message_flags(id, flags).await
    }
}

#[tokio::test]
async fn forked_payloads_have_independent_ids_flags_and_compaction_lifecycle() {
    use peri_acp_types::projection::{
        MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    };

    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteThreadStore::new(temp.path().join("fork-ownership.db"))
            .await
            .unwrap(),
    );
    let source_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
    let source_messages = [
        BaseMessage::human("source question"),
        BaseMessage::tool_result("read-1", "source tool output"),
    ];
    let source_payloads = source_messages
        .iter()
        .cloned()
        .map(PersistedPayload::Message)
        .collect::<Vec<_>>();
    store
        .append_payloads(&source_id, &source_payloads)
        .await
        .unwrap();
    let source_tool_id = source_messages[1].id();
    store
        .update_message_flags(
            &source_tool_id,
            &MessageFlags {
                truncated: true,
                excluded: false,
                projection: Some(MessageProjectionDirective {
                    policy_version: peri_agent::agent::compact_v2::PROJECTION_POLICY_VERSION,
                    entries: vec![ProjectionActionEntry {
                        message_id: source_tool_id,
                        target: ProjectionTarget::Message,
                        action: ProjectionAction::CompactToolResult {
                            keep_head: 8,
                            keep_tail: 8,
                            preserve_recovery_handle: true,
                        },
                    }],
                }),
            },
        )
        .await
        .unwrap();
    let controller = Controller::new(store.clone());

    let (fork_id, copied_payloads) =
        fork_session(&controller, &source_id, &source_payloads, "/tmp")
            .await
            .unwrap();

    assert_eq!(copied_payloads.len(), source_payloads.len());
    assert!(source_payloads
        .iter()
        .zip(&copied_payloads)
        .all(|(source, copied)| source.id() != copied.id()));
    assert_eq!(
        copied_payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        source_messages
            .iter()
            .map(BaseMessage::content)
            .collect::<Vec<_>>()
    );
    let stored_fork = store.load_payloads(&fork_id).await.unwrap();
    assert_eq!(
        stored_fork
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        copied_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    let copied_tool_id = copied_payloads[1].id();
    let copied_flags = store.load_message_flags(&fork_id).await.unwrap();
    let copied_tool_flags = &copied_flags[&copied_tool_id];
    assert!(copied_tool_flags.truncated);
    assert_eq!(
        copied_tool_flags.projection.as_ref().unwrap().entries[0].message_id,
        copied_tool_id,
        "fork 后 projection directive 必须引用新的消息 ID"
    );

    store
        .commit_compaction_lifecycle(
            &fork_id,
            &CompactionLifecycle {
                flag_updates: copied_payloads
                    .iter()
                    .map(|payload| {
                        (
                            payload.id(),
                            MessageFlags {
                                excluded: true,
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
                appended_messages: vec![BaseMessage::human("fork summary")],
            },
        )
        .await
        .expect("fork history 必须由新 thread 独立拥有并可提交 Full lifecycle");
    let source_flags = store.load_message_flags(&source_id).await.unwrap();
    assert!(source_flags[&source_tool_id].truncated);
    assert!(!source_flags[&source_tool_id].excluded);
}

#[tokio::test]
async fn payload_copy_failure_compensates_new_thread() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(FailingForkStore {
        inner: FilesystemThreadStore::new(temp.path()),
        fail_delete: false,
    });
    let controller = Controller::new(store.clone());

    let error = fork_session(
        &controller,
        "source",
        &[PersistedPayload::Message(BaseMessage::human("history"))],
        "/tmp",
    )
    .await
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("Failed to copy session payloads"));
    assert!(store.list_threads().await.unwrap().is_empty());
}

#[tokio::test]
async fn compensation_failure_returns_and_logs_redacted_inconsistency() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(FailingForkStore {
        inner: FilesystemThreadStore::new(temp.path()),
        fail_delete: true,
    });
    let controller = Controller::new(store.clone());

    let error = fork_session(
        &controller,
        "source",
        &[PersistedPayload::Message(BaseMessage::human("history"))],
        "/tmp",
    )
    .await
    .unwrap_err();
    let message = error.to_string();
    let captured = logs.contents();

    for sensitive in [
        COPY_CANARY,
        CLEANUP_CANARY,
        "/private/secret-copy.db",
        "/private/secret-cleanup.db",
        "postgres://user:pass@host/copy",
        "postgres://user:pass@host/cleanup",
    ] {
        assert!(!message.contains(sensitive));
        assert!(!captured.contains(sensitive));
    }
    assert!(message.contains("persistence inconsistency"));
    assert!(captured.contains("session_fork_persistence_inconsistency"));
    assert!(captured.contains("classification=\"persistence_inconsistency\""));
    assert!(captured.contains("copy_failed=true"));
    assert!(captured.contains("compensation_failed=true"));
    assert!(captured.contains("source_thread_id=\"source\""));
    assert!(captured.contains("new_thread_id="));
    assert_eq!(store.list_threads().await.unwrap().len(), 1);
}
