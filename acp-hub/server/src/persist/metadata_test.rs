use sqlx::{Connection, Executor};
use tempfile::tempdir;

use super::metadata::{payload_hash, BeginCommand, MetadataError, MetadataStore, NewSession};

#[tokio::test]
async fn v2_catalog_migrates_additively_to_current_schema() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.sqlite3");
    let mut connection =
        sqlx::SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
    connection
        .execute(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
        )
        .await
        .unwrap();
    connection
        .execute("INSERT INTO schema_migrations VALUES(1,'t'),(2,'t')")
        .await
        .unwrap();
    connection
        .execute(
            "CREATE TABLE project_sessions(\
             id TEXT PRIMARY KEY,project_id TEXT NOT NULL,acp_session_id TEXT UNIQUE,\
             acp_title TEXT,custom_name TEXT,lifecycle TEXT NOT NULL,created_at TEXT NOT NULL,\
             updated_at TEXT NOT NULL,last_opened_at TEXT,last_chat_id TEXT,failure_code TEXT,\
             origin TEXT NOT NULL DEFAULT 'legacy_hidden')",
        )
        .await
        .unwrap();
    connection.close().await.unwrap();

    let store = MetadataStore::open(dir.path()).await.unwrap();
    assert!(
        !store.seed_hub_title("unknown", "Title").await.unwrap(),
        "the additive column must be queryable without rebuilding user data"
    );
    assert!(
        store.session("unknown").await.unwrap().is_none(),
        "the archive column must also be queryable after an additive migration"
    );
    assert!(store.session_runtimes("unknown").await.unwrap().is_empty());
}

#[tokio::test]
async fn fresh_open_reopen_and_crud() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    let p = store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    assert_eq!(p.name, "Demo");
    let s = store
        .create_pending_session("s1", "p1", Some("ACP title"))
        .await
        .unwrap();
    assert_eq!(s.display_title(), "ACP title");
    store.rename_session("s1", "Alias").await.unwrap();
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().display_title(),
        "Alias"
    );
    drop(store);
    let reopened = MetadataStore::open(dir.path()).await.unwrap();
    assert_eq!(reopened.list_projects().await.unwrap().len(), 1);
    assert_eq!(reopened.list_sessions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn runtime_history_is_append_only_identity_not_liveness() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .create_pending_session("s1", "p1", None)
        .await
        .unwrap();
    store.record_session_runtime("s1", "chat-1").await.unwrap();
    store.record_session_runtime("s1", "chat-1").await.unwrap();
    store.record_session_runtime("s1", "chat-2").await.unwrap();
    let history = store.session_runtimes("s1").await.unwrap();
    assert_eq!(history.len(), 2, "same runtime provenance is idempotent");
    assert!(history.iter().all(|record| record.retired_at.is_none()));

    store.recover_after_restart().await.unwrap();
    assert_eq!(
        store.session_runtimes("s1").await.unwrap().len(),
        2,
        "restart clears active hints, not historical provenance"
    );
}

#[tokio::test]
async fn v4_migration_backfills_the_last_known_runtime_before_restart_clears_the_hint() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.sqlite3");
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .create_pending_session("s1", "p1", None)
        .await
        .unwrap();
    store
        .finalize_session("s1", "acp-1", None, "chat-before-v5")
        .await
        .unwrap();
    drop(store);

    let mut connection =
        sqlx::SqliteConnection::connect(&format!("sqlite://{}?mode=rw", path.display()))
            .await
            .unwrap();
    connection
        .execute("DROP INDEX session_runtime_history_session_activated_idx")
        .await
        .unwrap();
    connection
        .execute("DROP TABLE session_runtime_history")
        .await
        .unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version=5")
        .await
        .unwrap();
    connection.close().await.unwrap();

    let migrated = MetadataStore::open(dir.path()).await.unwrap();
    let history = migrated.session_runtimes("s1").await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].chat_id, "chat-before-v5");
}

#[tokio::test]
async fn project_archive_is_reversible_without_losing_sessions() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .import_session("s1", "p1", "acp-1", "Saved work", "2026-08-13T00:00:00Z")
        .await
        .unwrap();

    store.archive_project("p1").await.unwrap();
    assert!(store
        .project("p1")
        .await
        .unwrap()
        .unwrap()
        .archived_at
        .is_some());
    store.restore_project("p1").await.unwrap();
    assert!(store
        .project("p1")
        .await
        .unwrap()
        .unwrap()
        .archived_at
        .is_none());
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().display_title(),
        "Saved work"
    );
    assert!(
        store.restore_project("p1").await.is_err(),
        "restoring an active project is not an idempotent mutation"
    );
}

#[tokio::test]
async fn session_archive_is_reversible_and_preserves_runtime_lifecycle() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .import_session("s1", "p1", "acp-1", "Saved work", "2026-08-13T00:00:00Z")
        .await
        .unwrap();
    let lifecycle = store.session("s1").await.unwrap().unwrap().lifecycle;

    store.archive_session("s1").await.unwrap();
    let archived = store.session("s1").await.unwrap().unwrap();
    assert!(archived.archived_at.is_some());
    assert_eq!(archived.lifecycle, lifecycle);

    store.restore_session("s1").await.unwrap();
    let restored = store.session("s1").await.unwrap().unwrap();
    assert!(restored.archived_at.is_none());
    assert_eq!(restored.lifecycle, lifecycle);
    assert_eq!(restored.acp_session_id.as_deref(), Some("acp-1"));
}

#[tokio::test]
async fn session_restore_requires_an_active_parent_project() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .import_session("s1", "p1", "acp-1", "Saved work", "2026-08-13T00:00:00Z")
        .await
        .unwrap();
    store.archive_session("s1").await.unwrap();
    store.archive_project("p1").await.unwrap();
    assert!(matches!(
        store.restore_session("s1").await,
        Err(MetadataError::InvalidState(_))
    ));
}

#[tokio::test]
async fn project_rename_preserves_directory_and_rejects_empty_or_archived_projects() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    let cwd = dir.path().to_str().unwrap();
    store
        .create_project("p1", "Before", cwd, "local")
        .await
        .unwrap();
    store.rename_project("p1", "  After  ").await.unwrap();
    let renamed = store.project("p1").await.unwrap().unwrap();
    assert_eq!(renamed.name, "After");
    assert_eq!(renamed.cwd, cwd);
    assert!(store.rename_project("p1", "  ").await.is_err());
    store.archive_project("p1").await.unwrap();
    assert!(store.rename_project("p1", "Hidden").await.is_err());
}

#[tokio::test]
async fn acp_title_refresh_is_exact_idempotent_and_preserves_user_alias() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .import_session("s1", "p1", "acp-1", "Old ACP title", "2026-08-13T00:00:00Z")
        .await
        .unwrap();
    store.rename_session("s1", "My alias").await.unwrap();
    let generation_before = store.generation().await.unwrap().0;

    assert_eq!(
        store
            .update_acp_titles(&[
                ("acp-1".into(), "New ACP title".into()),
                ("unknown".into(), "Ignored".into()),
                ("acp-1".into(), "".into()),
            ])
            .await
            .unwrap(),
        1
    );
    let refreshed = store.session("s1").await.unwrap().unwrap();
    assert_eq!(refreshed.acp_title.as_deref(), Some("New ACP title"));
    assert_eq!(refreshed.display_title(), "My alias");
    assert_eq!(store.generation().await.unwrap().0, generation_before + 1);

    assert_eq!(
        store
            .update_acp_titles(&[("acp-1".into(), "New ACP title".into())])
            .await
            .unwrap(),
        0
    );
    assert_eq!(store.generation().await.unwrap().0, generation_before + 1);
}

#[tokio::test]
async fn hub_prompt_title_is_one_shot_and_never_outranks_owned_titles() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "c1",
            "session/create",
            "h1",
            Some("p1"),
            Some("s1"),
            Some(NewSession {
                id: "s1",
                project_id: "p1",
                title: None,
            }),
            Some("s1"),
        )
        .await
        .unwrap();
    store
        .activation_phase("s1", "acp_id_durable", Some("chat1"), Some("acp1"))
        .await
        .unwrap();
    store
        .finalize_session_and_command("c1", "s1", "p1", "acp1", None, "chat1")
        .await
        .unwrap();

    assert!(store.seed_hub_title("acp1", "First task").await.unwrap());
    assert!(!store.seed_hub_title("acp1", "Second task").await.unwrap());
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().display_title(),
        "First task"
    );

    store.update_acp_title("acp1", "ACP title").await.unwrap();
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().display_title(),
        "ACP title"
    );
    store.rename_session("s1", "My alias").await.unwrap();
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().display_title(),
        "My alias"
    );
    assert!(
        !store.seed_hub_title("missing", "Ignored").await.unwrap(),
        "unknown and imported ACP ids must not create navigation facts"
    );
}

#[tokio::test]
async fn projection_snapshot_is_generation_consistent_and_watermark_monotonic() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    let snapshot = store.snapshot().await.unwrap();
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.sessions.len(), 0);

    store.mark_projected(snapshot.generation).await.unwrap();
    store.mark_projected(snapshot.generation - 1).await.unwrap();
    assert_eq!(
        store.generation().await.unwrap().1,
        snapshot.generation,
        "an older projection completion cannot move the watermark backwards"
    );
}

#[tokio::test]
async fn command_dedup_detects_payload_mismatch_and_replays_result() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    assert_eq!(
        store
            .begin_command("c1", "project/create", "h1", None, None)
            .await
            .unwrap(),
        BeginCommand::New
    );
    assert_eq!(
        store
            .begin_command("c1", "project/create", "h1", None, None)
            .await
            .unwrap(),
        BeginCommand::Existing
    );
    assert!(matches!(
        store
            .begin_command("c1", "project/create", "h2", None, None)
            .await,
        Err(MetadataError::Conflict(_))
    ));
    store
        .update_command("c1", "committed", Some("p1"), None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .command("c1")
            .await
            .unwrap()
            .unwrap()
            .project_id
            .as_deref(),
        Some("p1")
    );
}

#[tokio::test]
async fn activation_restart_before_dispatch_is_safe_failure() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .create_pending_session("s1", "p1", None)
        .await
        .unwrap();
    store
        .begin_command("c1", "session/create", "h", Some("p1"), Some("s1"))
        .await
        .unwrap();
    store.begin_activation("s1", "c1").await.unwrap();
    assert_eq!(
        store
            .stale_activations_require_reconciliation()
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().lifecycle,
        "failed"
    );
}

#[test]
fn payload_hash_is_stable() {
    let value = serde_json::json!({"projectId":"p1","title":"x"});
    assert_eq!(payload_hash(&value).unwrap(), payload_hash(&value).unwrap());
}

#[tokio::test]
async fn atomic_create_and_simultaneous_open_lease() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "c1",
            "session/create",
            "h1",
            Some("p1"),
            Some("s1"),
            Some(NewSession {
                id: "s1",
                project_id: "p1",
                title: Some("Title"),
            }),
            Some("s1"),
        )
        .await
        .unwrap();
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().lifecycle,
        "activating"
    );
    store
        .activation_phase("s1", "acp_id_durable", Some("chat1"), Some("acp1"))
        .await
        .unwrap();
    store
        .finalize_session_and_command("c1", "s1", "p1", "acp1", Some("Title"), "chat1")
        .await
        .unwrap();
    assert_eq!(
        store.command("c1").await.unwrap().unwrap().phase,
        "projection_pending"
    );
    store
        .update_command(
            "c1",
            "committed",
            Some("p1"),
            Some("s1"),
            Some("chat1"),
            Some("acp1"),
            None,
        )
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "o1",
            "session/open",
            "h2",
            None,
            Some("s1"),
            None,
            Some("s1"),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .begin_command_with_activation(
                "o2",
                "session/open",
                "h3",
                None,
                Some("s1"),
                None,
                Some("s1")
            )
            .await,
        Err(MetadataError::Conflict(_))
    ));
    assert!(
        store.command("o2").await.unwrap().is_none(),
        "losing lease transaction must roll back command intention"
    );
}

#[tokio::test]
async fn restart_recovers_durable_acp_id_and_clears_runtime_chat() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "c1",
            "session/create",
            "h1",
            Some("p1"),
            Some("s1"),
            Some(NewSession {
                id: "s1",
                project_id: "p1",
                title: None,
            }),
            Some("s1"),
        )
        .await
        .unwrap();
    store
        .activation_phase("s1", "acp_id_durable", Some("dead-chat"), Some("acp1"))
        .await
        .unwrap();
    let (recovered, reconciled) = store.recover_after_restart().await.unwrap();
    assert_eq!((recovered, reconciled), (1, 0));
    let session = store.session("s1").await.unwrap().unwrap();
    assert_eq!(session.lifecycle, "ready");
    assert_eq!(session.acp_session_id.as_deref(), Some("acp1"));
    assert!(session.last_chat_id.is_none());
}

#[tokio::test]
async fn owner_lock_is_exclusive_and_db_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    assert!(matches!(
        MetadataStore::open(dir.path()).await,
        Err(MetadataError::Conflict(_))
    ));
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    for name in [
        "metadata.sqlite3",
        "metadata.sqlite3-wal",
        "metadata.sqlite3-shm",
        "metadata.owner.lock",
    ] {
        let path = dir.path().join(name);
        if path.exists() {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    drop(store);
    MetadataStore::open(dir.path()).await.unwrap();
}

#[tokio::test]
async fn restart_terminates_pre_dispatch_intention_as_safe_retry() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "c1",
            "session/create",
            "h",
            Some("p1"),
            Some("s1"),
            Some(NewSession {
                id: "s1",
                project_id: "p1",
                title: None,
            }),
            Some("s1"),
        )
        .await
        .unwrap();
    store.recover_after_restart().await.unwrap();
    let command = store.command("c1").await.unwrap().unwrap();
    assert_eq!(command.phase, "failed");
    assert_eq!(
        command.error_code.as_deref(),
        Some("server_restart_before_dispatch_safe_retry")
    );
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().lifecycle,
        "failed"
    );
}

#[tokio::test]
async fn dispatched_restart_is_reconciliation_not_safe_retry() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "c1",
            "session/create",
            "h",
            Some("p1"),
            Some("s1"),
            Some(NewSession {
                id: "s1",
                project_id: "p1",
                title: None,
            }),
            Some("s1"),
        )
        .await
        .unwrap();
    store
        .activation_phase("s1", "dispatched", None, None)
        .await
        .unwrap();
    store
        .update_command("c1", "dispatched", Some("p1"), Some("s1"), None, None, None)
        .await
        .unwrap();
    let (_, reconciled) = store.recover_after_restart().await.unwrap();
    assert_eq!(reconciled, 1);
    assert_eq!(
        store.command("c1").await.unwrap().unwrap().phase,
        "reconciliation_required"
    );
}

#[tokio::test]
async fn unknown_activation_atomically_reconciles_command_and_session() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .begin_command_with_activation(
            "c1",
            "session/create",
            "h",
            Some("p1"),
            Some("s1"),
            Some(NewSession {
                id: "s1",
                project_id: "p1",
                title: None,
            }),
            Some("s1"),
        )
        .await
        .unwrap();
    store
        .reconcile_activation_and_command("s1", "c1", "channel_closed")
        .await
        .unwrap();
    assert_eq!(
        store.command("c1").await.unwrap().unwrap().phase,
        "reconciliation_required"
    );
    assert_eq!(
        store.session("s1").await.unwrap().unwrap().lifecycle,
        "reconciliation_required"
    );
}

#[tokio::test]
async fn explicit_import_promotes_hidden_session_and_preserves_single_identity() {
    let dir = tempdir().unwrap();
    let store = MetadataStore::open(dir.path()).await.unwrap();
    store
        .create_project("p1", "Demo", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    store
        .import_session("legacy", "p1", "acp1", "Old", "2026-08-01T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(
        store.session("legacy").await.unwrap().unwrap().origin,
        "legacy_hidden"
    );

    let imported = store
        .import_explicit_session("new-id", "p1", "acp1", "Imported", "2026-08-12T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(imported.id, "legacy");
    assert_eq!(imported.origin, "imported");
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);
    store
        .create_project("p2", "Other", dir.path().to_str().unwrap(), "local")
        .await
        .unwrap();
    assert!(matches!(
        store
            .import_explicit_session("other", "p2", "acp1", "Moved", "")
            .await,
        Err(MetadataError::Conflict(_))
    ));
}
