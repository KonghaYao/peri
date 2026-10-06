use std::{path::Path, sync::Arc};

use peri_acp_types::{
    identity::AttemptId,
    session::TurnId,
    session_resources::{
        ControlAction, ControlAttempt, ControlCommand, ControlDecision, ControlReceipt,
        ControlRejection, ControlResolution, ControlState, ControlStatus, FrozenSnapshotBytes,
        NewSession, NewSessionMeta, SessionResourceErrorKind, SessionResources,
    },
    workspace::SessionBinding,
};
use tempfile::TempDir;

async fn open(path: &Path) -> Arc<dyn SessionResources> {
    Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(path)
            .await
            .unwrap(),
    )
}

async fn fixture() -> (TempDir, Arc<dyn SessionResources>) {
    let directory = tempfile::tempdir().unwrap();
    let resources = open(&directory.path().join("sessions.db")).await;
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: "control-session".into(),
            created_at: "2026-10-05T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    (directory, resources)
}

fn command(command_id: &str, state: &ControlState, action: ControlAction) -> ControlCommand {
    ControlCommand {
        session_id: "control-session".into(),
        command_id: command_id.into(),
        expected_lifecycle: state.lifecycle,
        expected_revision: state.revision,
        expected_control_generation: state.control_generation,
        action,
    }
}

async fn apply(
    resources: &dyn SessionResources,
    command_id: &str,
    action: ControlAction,
) -> ControlReceipt {
    let state = resources
        .load_session_control(&"control-session".into())
        .await
        .unwrap();
    resources
        .apply_session_control(&command(command_id, &state, action))
        .await
        .unwrap()
}

#[tokio::test]
async fn control_replay_returns_original_receipt_after_resume_and_reopen() {
    let (directory, resources) = fixture().await;
    let pause = command("pause", &ControlState::default(), ControlAction::Pause);
    let receipt = resources.apply_session_control(&pause).await.unwrap();
    apply(resources.as_ref(), "resume", ControlAction::Resume).await;
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert_eq!(
        reopened.apply_session_control(&pause).await.unwrap(),
        receipt
    );
    assert_eq!(
        reopened.resolve_session_control(&pause).await.unwrap(),
        ControlResolution::Applied { receipt }
    );
    assert_eq!(
        reopened
            .load_session_control(&pause.session_id)
            .await
            .unwrap()
            .status,
        ControlStatus::Active
    );
}

#[tokio::test]
async fn control_conflicting_id_rejects_without_state_change() {
    let (_directory, resources) = fixture().await;
    let pause = command("same", &ControlState::default(), ControlAction::Pause);
    let receipt = resources.apply_session_control(&pause).await.unwrap();
    let conflict = command("same", &receipt.state, ControlAction::Resume);
    assert!(matches!(
        resources
            .apply_session_control(&conflict)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert!(matches!(
        resources
            .resolve_session_control(&conflict)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        resources
            .load_session_control(&pause.session_id)
            .await
            .unwrap(),
        receipt.state
    );
}

#[tokio::test]
async fn control_stale_revision_rejection_is_durable_and_replayable() {
    let (directory, resources) = fixture().await;
    apply(resources.as_ref(), "first", ControlAction::Pause).await;
    let stale = command("stale", &ControlState::default(), ControlAction::Resume);
    let receipt = resources.apply_session_control(&stale).await.unwrap();
    assert_eq!(
        receipt.decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleRevision
        }
    );
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert_eq!(
        reopened.apply_session_control(&stale).await.unwrap(),
        receipt
    );
}

#[tokio::test]
async fn control_stale_generation_rejects_even_at_current_revision() {
    let (_directory, resources) = fixture().await;
    let paused = apply(resources.as_ref(), "pause", ControlAction::Pause).await;
    let mut stale = command("old-generation", &paused.state, ControlAction::Resume);
    stale.expected_control_generation = 0;
    assert_eq!(
        resources
            .apply_session_control(&stale)
            .await
            .unwrap()
            .decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleControlGeneration
        }
    );
}

#[tokio::test]
async fn control_stop_requires_both_turn_and_attempt() {
    let (_directory, resources) = fixture().await;
    let target = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let observed = apply(
        resources.as_ref(),
        "observe",
        ControlAction::ObserveAttempt {
            target: Some(target.clone()),
        },
    )
    .await;
    let wrong = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: target.attempt_id.clone(),
    };
    let stale = command(
        "wrong-turn",
        &observed.state,
        ControlAction::Stop { target: wrong },
    );
    assert_eq!(
        resources
            .apply_session_control(&stale)
            .await
            .unwrap()
            .decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleAttempt
        }
    );
    let stopped = apply(resources.as_ref(), "stop", ControlAction::Stop { target }).await;
    assert_eq!(stopped.state.status, ControlStatus::Paused);
}

#[tokio::test]
async fn control_observe_cannot_replace_an_existing_attempt_without_cas_clear() {
    let (_directory, resources) = fixture().await;
    let first = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let observed = apply(
        resources.as_ref(),
        "first-attempt",
        ControlAction::ObserveAttempt {
            target: Some(first),
        },
    )
    .await;
    let next = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let replace = command(
        "replace-attempt",
        &observed.state,
        ControlAction::ObserveAttempt {
            target: Some(next.clone()),
        },
    );
    let rejected = resources.apply_session_control(&replace).await.unwrap();
    assert_eq!(
        rejected.decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleAttempt
        }
    );
    assert_eq!(rejected.state, observed.state);
    apply(
        resources.as_ref(),
        "clear-attempt",
        ControlAction::ObserveAttempt { target: None },
    )
    .await;
    let recorded = apply(
        resources.as_ref(),
        "next-attempt",
        ControlAction::ObserveAttempt {
            target: Some(next.clone()),
        },
    )
    .await;
    assert_eq!(recorded.state.attempt, Some(next));
}

#[tokio::test]
async fn control_close_is_atomic_with_admission_intent_and_separate_finish() {
    let (directory, resources) = fixture().await;
    let close = command("close", &ControlState::default(), ControlAction::Close);
    let closing = resources.apply_session_control(&close).await.unwrap();
    assert_eq!(closing.state.status, ControlStatus::Closing);
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert!(reopened
        .is_session_closing(&close.session_id)
        .await
        .unwrap());
    assert_eq!(
        reopened
            .load_session_control(&close.session_id)
            .await
            .unwrap(),
        closing.state
    );
    let closed = apply(reopened.as_ref(), "finish", ControlAction::FinishClose).await;
    assert_eq!(closed.state.status, ControlStatus::Closed);
    assert_eq!(
        reopened.apply_session_control(&close).await.unwrap(),
        closing
    );
    let active = apply(reopened.as_ref(), "reopen", ControlAction::Reopen).await;
    assert_eq!(active.state.lifecycle, 2);
    assert_eq!(active.state.status, ControlStatus::Active);
    assert!(!reopened
        .is_session_closing(&close.session_id)
        .await
        .unwrap());
    let old = command("old-lifecycle", &closed.state, ControlAction::Pause);
    assert_eq!(
        reopened.apply_session_control(&old).await.unwrap().decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleLifecycle
        }
    );
}

#[tokio::test]
async fn control_tombstone_can_finish_after_canonical_session_delete() {
    let (directory, resources) = fixture().await;
    let closing = apply(resources.as_ref(), "close", ControlAction::Close).await;
    resources
        .delete_session_tree(&closing.session_id)
        .await
        .unwrap();
    let finish = command("finish-deleted", &closing.state, ControlAction::FinishClose);
    let closed = resources.apply_session_control(&finish).await.unwrap();
    assert_eq!(closed.state.status, ControlStatus::Closed);
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert_eq!(
        reopened
            .load_session_control(&closing.session_id)
            .await
            .unwrap(),
        closed.state
    );
    assert_eq!(
        reopened.apply_session_control(&finish).await.unwrap(),
        closed
    );
}

#[tokio::test]
async fn control_resolution_seals_original_id_against_late_apply() {
    let (directory, resources) = fixture().await;
    let original = command("late", &ControlState::default(), ControlAction::Pause);
    assert_eq!(
        resources.resolve_session_control(&original).await.unwrap(),
        ControlResolution::NotApplied
    );
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert!(matches!(
        reopened
            .apply_session_control(&original)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        reopened
            .load_session_control(&original.session_id)
            .await
            .unwrap(),
        ControlState::default()
    );
}

#[tokio::test]
async fn control_two_store_instances_cas_accept_only_one_revision() {
    let (directory, resources) = fixture().await;
    let other = open(&directory.path().join("sessions.db")).await;
    let pause = command("race-pause", &ControlState::default(), ControlAction::Pause);
    let close = command("race-close", &ControlState::default(), ControlAction::Close);
    let (first, second) = tokio::join!(
        resources.apply_session_control(&pause),
        other.apply_session_control(&close)
    );
    let receipts = [first.unwrap(), second.unwrap()];
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.decision == ControlDecision::Accepted)
            .count(),
        1
    );
    assert_eq!(
        resources
            .load_session_control(&pause.session_id)
            .await
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn control_read_only_store_refuses_apply_and_resolution() {
    let (directory, _resources) = fixture().await;
    let readonly = peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(
        &directory.path().join("sessions.db"),
    )
    .await
    .unwrap();
    let pause = command("readonly", &ControlState::default(), ControlAction::Pause);
    assert!(matches!(
        readonly
            .apply_session_control(&pause)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    assert!(matches!(
        readonly
            .resolve_session_control(&pause)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
}

#[tokio::test]
async fn control_v14_refusal_preserves_existing_close_intent() {
    use sqlx::Connection;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE IF NOT EXISTS machines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    identity_kind TEXT NOT NULL CHECK(identity_kind IN ('known', 'legacy_unknown'))
);
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    UNIQUE(machine_id, path)
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))
);
CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity)
);
CREATE TABLE IF NOT EXISTS legacy_execution_registrations (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id)
);
CREATE TABLE IF NOT EXISTS session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT,
    evidence_origin TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES legacy_execution_registrations(id, project_id)
);
CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(principal_id, workspace_id, server_key)
);
CREATE TABLE IF NOT EXISTS session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
);"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::raw_sql("INSERT INTO machines VALUES ('machine','legacy','known'); INSERT INTO workspaces VALUES ('workspace','machine','/old','unverified'); INSERT INTO threads(id,created_at,updated_at,workspace_id) VALUES ('control-session','now','now','workspace'); INSERT INTO session_close_intents VALUES ('control-session','legacy close timestamp'); PRAGMA user_version=14").execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true),
    )
    .await
    .unwrap();
    let before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name,sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    connection.close().await.unwrap();
    let error = peri_resources::sessions::SqliteThreadStore::new(&path)
        .await
        .err()
        .expect("v14 must not auto-upgrade");
    assert!(error
        .to_string()
        .contains("explicit stopped-writer offline migration"));

    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true),
    )
    .await
    .unwrap();
    let after: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name,sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after, before);
    let intent: String = sqlx::query_scalar(
        "SELECT requested_at FROM session_close_intents WHERE thread_id='control-session'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(intent, "legacy close timestamp");
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 14);
    let control_tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('session_control_state','session_control_receipts')").fetch_one(&mut connection).await.unwrap();
    assert_eq!(control_tables, 0);
}
