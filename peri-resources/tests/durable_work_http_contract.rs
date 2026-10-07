use std::{net::TcpListener, path::Path, process::Child, sync::Arc, time::Duration};

use peri_acp_types::{
    messages::BaseMessage,
    session::MessagePolicy,
    session_resources::{
        work::*, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
        SessionStoreShutdownPort,
    },
    session_store::SessionStoreDeployment,
    store::PersistedPayload,
    workspace::SessionBinding,
};
use peri_resources::{sessions::RemoteWorkspaceEnvironment, Resources};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn open(deployment: &SessionStoreDeployment, root: &Path) -> Resources {
    Resources::open_deployment_in_remote_environment(
        deployment,
        RemoteWorkspaceEnvironment::virtual_workspace(
            "00000000-0000-4000-8000-000000000003",
            root.to_path_buf(),
        )
        .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires an explicitly supplied real libSQL server binary PERI_TEST_SQLD"]
async fn real_http_libsql_fresh_open_retains_original_commands_across_facade_restart() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut server = Server(
        std::process::Command::new(std::env::var("PERI_TEST_SQLD").unwrap())
            .args([
                "--no-welcome",
                "--http-listen-addr",
                &address.to_string(),
                "--db-path",
            ])
            .arg(directory.path().join("server"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut listening = false;
    for _attempt in 0..100 {
        assert!(server.0.try_wait().unwrap().is_none());
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            listening = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(listening);
    let credential_key = format!("PERI_HTTP_CONTRACT_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&credential_key, "local-dev");
    let deployment = SessionStoreDeployment::from_locator(format!("http://{address}"))
        .with_engine("libsql")
        .with_credential_env(&credential_key);
    let resources = open(&deployment, directory.path()).await;
    let (store, shutdown) = resources.into_parts();
    let workspace = store.resolve_workspace(directory.path()).await.unwrap();
    store
        .create_session(&NewSession {
            thread_id: "http-work".into(),
            created_at: "2026-10-06T00:00:00Z".into(),
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
    let original = WorkCommand {
        session_id: "http-work".into(),
        recipient_lifecycle: 1,
        mutation_id: "http-original-command".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "http-required".into(),
                event: WorkEvent {
                    producer_namespace: "http-contract".into(),
                    event_id: "http-event".into(),
                    event_kind: "input".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::human("retained original HTTP body"),
                    ))
                    .unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    };
    let receipt = store.apply_work_mutation(&original).await.unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    shutdown.shutdown().await.unwrap();
    drop(store);
    drop(shutdown);
    let (reopened, shutdown) = open(&deployment, directory.path()).await.into_parts();
    verify_original(&reopened, &original, &receipt).await;
    let never_sent = WorkCommand {
        mutation_id: "http-before-send".into(),
        ..original.clone()
    };
    assert_eq!(
        reopened.resolve_work_mutation(&never_sent).await.unwrap(),
        WorkResolution::NotApplied
    );
    let saved = reopened
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: never_sent.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.command, never_sent);
    assert_eq!(saved.resolution, Some(WorkResolution::NotApplied));
    assert!(!saved.pending);
    let snapshot = reopened
        .load_session_work(&WorkQuery {
            session_id: original.session_id.clone(),
            limit: 64,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.deliveries.len(), 1);
    assert_eq!(snapshot.state.obligations.len(), 1);
    assert!(!snapshot.blocked);
    shutdown.shutdown().await.unwrap();
    std::env::remove_var(credential_key);
}

async fn verify_original(
    store: &Arc<dyn SessionResources>,
    original: &WorkCommand,
    receipt: &WorkReceipt,
) {
    let saved = store
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.command, *original);
    assert_eq!(
        saved.resolution,
        Some(WorkResolution::Applied {
            receipt: receipt.clone()
        })
    );
    assert!(!saved.pending);
    assert_eq!(store.apply_work_mutation(original).await.unwrap(), *receipt);
    assert_eq!(
        store.resolve_work_mutation(original).await.unwrap(),
        WorkResolution::Applied {
            receipt: receipt.clone()
        }
    );
    let conflicting = WorkCommand {
        recipient_lifecycle: 2,
        ..original.clone()
    };
    assert!(store.apply_work_mutation(&conflicting).await.is_err());
}
