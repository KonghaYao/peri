//! Remote/local dispatch for a child's session store (S6b).
//!
//! The remote branch needs a live Loom (verified in S6b2); these tests only pin
//! the refusal to degrade: a half-configured remote identity must not fall back
//! to the local SQLite file.

use std::path::PathBuf;

use uuid::Uuid;

use super::{open_session_resources, ChildConfig, ChildError, SessionStoreDeployment};

fn config(machine_id: Option<Uuid>, workspace_root: Option<PathBuf>) -> ChildConfig {
    ChildConfig {
        cwd: PathBuf::from("/tmp/peri-swarm-runtime-test"),
        session_store: SessionStoreDeployment::default_local(),
        machine_id,
        workspace_root,
        settings_json: None,
        model: None,
        instructions: None,
        root_session_id: "0192f0c4-0000-7000-8000-000000000002".to_owned(),
    }
}

async fn expect_partial_identity_error(machine_id: Option<Uuid>, workspace_root: Option<PathBuf>) {
    let error = open_session_resources(&config(machine_id, workspace_root))
        .await
        .expect_err("a partial remote identity must not open any store");
    match error {
        ChildError::Local(message) => {
            assert!(message.contains("machine_id"), "{message}");
            assert!(message.contains("workspace_root"), "{message}");
            assert!(message.contains("refusing"), "{message}");
        }
        other => panic!("expected a local configuration error, got {other:?}"),
    }
}

#[tokio::test]
async fn machine_id_without_workspace_root_is_an_explicit_error() {
    expect_partial_identity_error(Some(Uuid::now_v7()), None).await;
}

#[tokio::test]
async fn workspace_root_without_machine_id_is_an_explicit_error() {
    expect_partial_identity_error(None, Some(PathBuf::from("/tmp/root"))).await;
}
