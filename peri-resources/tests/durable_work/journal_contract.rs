use super::*;
use peri_acp_types::session_resources::{ControlDecision, PersistenceRecovery};
use std::process::{Command, Stdio};

#[tokio::test]
async fn schema16_unowned_receipt_is_quarantined_without_guessing_original_command() {
    let (directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let database = directory.path().join("work.db");
    let connection = sqlx::SqlitePool::connect(&format!("sqlite:{}", database.display()))
        .await
        .unwrap();
    sqlx::raw_sql("DROP TABLE session_work_commands; PRAGMA user_version=16")
        .execute(&connection)
        .await
        .unwrap();
    drop(resources);
    let reopened = SessionResourcesImpl::open(database).await.unwrap();
    let loaded = snapshot(&reopened).await;
    assert!(loaded.blocked);
    assert!(loaded
        .state
        .legacy_unknown
        .contains_key("unowned-command-history"));
    assert_eq!(
        loaded.state.obligations["delivery"].status,
        ObligationStatus::Pending
    );
    assert!(loaded.pending_commands.is_empty());
    assert!(loaded.candidates.is_empty());
}

#[tokio::test]
#[ignore]
async fn owned_command_crash_child() {
    let database = std::env::var("PERI_WORK_CRASH_DATABASE").unwrap();
    let command_path = std::env::var("PERI_WORK_CRASH_COMMAND").unwrap();
    let ready_path = std::env::var("PERI_WORK_CRASH_READY").unwrap();
    let command: WorkCommand =
        serde_json::from_slice(&std::fs::read(command_path).unwrap()).unwrap();
    let resources = SessionResourcesImpl::open(database).await.unwrap();
    assert!(resources.apply_work_mutation(&command).await.is_err());
    std::fs::write(ready_path, b"original command durable").unwrap();
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn sqlite_owned_command_journal_survives_process_crash_before_and_after_effects() {
    for applied in [false, true] {
        let (directory, resources) = fixture().await;
        let initial = publish(resources.as_ref(), "delivery").await;
        let accepted_query = WorkCommandQuery {
            session_id: "work-session".into(),
            mutation_id: "publish-delivery".into(),
        };
        let accepted_original = resources
            .load_work_command(&accepted_query)
            .await
            .unwrap()
            .unwrap();
        let accepted_delivery =
            snapshot(resources.as_ref()).await.state.deliveries["delivery"].clone();
        let original = command(
            "crash-withdraw",
            WorkAction::WithdrawDelivery {
                expected_revision: initial.revision,
                expected_control_generation: None,
                delivery_id: "delivery".into(),
                authorization_ref: "trusted-original-withdraw".into(),
            },
        );
        let database = directory.path().join("work.db");
        let connection = sqlx::SqlitePool::connect(&format!("sqlite:{}", database.display()))
            .await
            .unwrap();
        let trigger = if applied {
            "CREATE TRIGGER crash_window BEFORE UPDATE ON session_work_commands WHEN NEW.mutation_id='crash-withdraw' AND NEW.reconciled=1 BEGIN SELECT RAISE(ABORT,'lost owned acknowledgement'); END"
        } else {
            "CREATE TRIGGER crash_window BEFORE UPDATE ON session_work_state BEGIN SELECT RAISE(ABORT,'before business effects'); END"
        };
        sqlx::query(trigger).execute(&connection).await.unwrap();
        let command_path = directory.path().join("original-command.json");
        let ready_path = directory.path().join("child-ready");
        std::fs::write(&command_path, serde_json::to_vec(&original).unwrap()).unwrap();
        drop(resources);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "journal_contract::owned_command_crash_child",
            ])
            .env("PERI_WORK_CRASH_DATABASE", &database)
            .env("PERI_WORK_CRASH_COMMAND", &command_path)
            .env("PERI_WORK_CRASH_READY", &ready_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let ready = peri_time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if ready_path.exists() {
                    break;
                }
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "child exited before durable command"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        child.kill().unwrap();
        child.wait().unwrap();
        ready.unwrap();
        std::fs::remove_file(&command_path).unwrap();
        sqlx::query("DROP TRIGGER crash_window")
            .execute(&connection)
            .await
            .unwrap();
        let reopened = SessionResourcesImpl::open(&database).await.unwrap();
        let loaded = snapshot(&reopened).await;
        assert!(loaded.blocked);
        assert_eq!(
            reopened.load_work_command(&accepted_query).await.unwrap(),
            Some(accepted_original)
        );
        assert_eq!(
            loaded.state.deliveries["delivery"].publication,
            accepted_delivery.publication
        );
        assert_eq!(
            loaded.state.deliveries["delivery"].projection,
            accepted_delivery.projection
        );
        assert_eq!(loaded.pending_commands, vec![original.clone()]);
        assert!(loaded.candidates.is_empty());
        assert_eq!(
            loaded.state.obligations["delivery"].status,
            if applied {
                ObligationStatus::Abandoned
            } else {
                ObligationStatus::Pending
            }
        );
        assert!(reopened
            .apply_work_mutation(&command(
                "replacement",
                WorkAction::PublishDelivery {
                    delivery: publication("replacement", MessagePolicy::ensure_processing()),
                }
            ))
            .await
            .unwrap_err()
            .is_persistence_uncertain());
        assert_eq!(
            reopened
                .recover_session_persistence(&original.session_id)
                .await
                .unwrap(),
            PersistenceRecovery::StillBlocked
        );
        let resolution = reopened
            .resolve_work_mutation(&loaded.pending_commands[0])
            .await
            .unwrap();
        if applied {
            assert!(matches!(resolution, WorkResolution::Applied { .. }));
        } else {
            assert_eq!(resolution, WorkResolution::NotApplied);
        }
        assert_eq!(
            reopened.resolve_work_mutation(&original).await.unwrap(),
            resolution
        );
        assert!(snapshot(&reopened).await.pending_commands.is_empty());
        let owned = reopened
            .load_work_command(&WorkCommandQuery {
                session_id: original.session_id.clone(),
                mutation_id: original.mutation_id.clone(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owned.command, original);
        assert_eq!(owned.resolution, Some(resolution.clone()));
        assert!(!owned.pending);
        assert!(reopened
            .load_work_command(&WorkCommandQuery {
                session_id: "other-session".into(),
                mutation_id: original.mutation_id.clone()
            })
            .await
            .unwrap()
            .is_none());
        let current = reopened
            .load_session_control(&original.session_id)
            .await
            .unwrap();
        let resumed = reopened
            .apply_session_control(&ControlCommand {
                session_id: original.session_id.clone(),
                command_id: "after-reconcile-pause".into(),
                expected_lifecycle: current.lifecycle,
                expected_revision: current.revision,
                expected_control_generation: current.control_generation,
                action: ControlAction::Pause,
            })
            .await
            .unwrap();
        assert_eq!(resumed.decision, ControlDecision::Accepted);
    }
}
