use super::*;
use peri_acp_types::store::ThreadStore;
use peri_resources::sessions::{SessionResourcesImpl, SqliteThreadStore};

pub(super) struct WorkBackend {
    pub(super) resources: Arc<SessionResourcesImpl>,
    store: SqliteThreadStore,
    registered: tokio::sync::Mutex<HashSet<String>>,
    _directory: tempfile::TempDir,
    pub(super) repository: tempfile::TempDir,
}

impl MockSessionResources {
    pub(super) async fn durable_backend(&self, session_id: &str) -> &SessionResourcesImpl {
        let backend = self
            .work_backend
            .get_or_init(|| async {
                let directory = tempfile::tempdir().unwrap();
                let (store, resources) = peri_resources::sessions::open_store_and_facade_for_tests(
                    directory.path().join("work.db"),
                )
                .await
                .unwrap();
                WorkBackend {
                    resources: Arc::new(resources),
                    store,
                    registered: tokio::sync::Mutex::new(HashSet::new()),
                    _directory: directory,
                    repository: crate::session::test_resources::git_repository(),
                }
            })
            .await;
        let mut registered = backend.registered.lock().await;
        if !registered.contains(session_id) {
            let mut meta = ThreadMeta::new_at(
                backend
                    .repository
                    .path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy(),
                peri_time::now_wall(),
            );
            meta.id = session_id.into();
            backend.store.create_thread(meta).await.unwrap();
            registered.insert(session_id.into());
        }
        &backend.resources
    }
}
