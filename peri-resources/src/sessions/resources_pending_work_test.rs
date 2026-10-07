use super::*;
use peri_acp_types::session_resources::work::{ResourceOwnerBinding, WorkQuery, WorkState};

#[tokio::test]
async fn resource_owner_projection_preserves_missing_and_corrupt_facts() {
    let fixture = Fixture::new().await;
    fixture.create("owner-facts").await;
    let id = "owner-facts".to_owned();
    let missing = fixture
        .facade
        .load_resource_owner_facts(&id, 2)
        .await
        .unwrap();
    assert_eq!(missing.control.lifecycle, 1);
    assert_eq!(missing.revision, 0);
    assert!(missing.current_owner.is_none());
    assert!(matches!(
        fixture
            .facade
            .load_resource_owner_facts(&"absent".into(), 0)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));

    let mut state = WorkState {
        revision: 7,
        ..Default::default()
    };
    let owner = ResourceOwnerBinding {
        recipient_lifecycle: 1,
        connections_json: "{}".into(),
        authorization_ref: "trusted".into(),
    };
    state.resource_owners.insert(1, owner.clone());
    state
        .child_resume_metadata
        .insert(1, "{\"version\":1}".into());
    sqlx::query("INSERT OR REPLACE INTO session_work_state(session_id,state_json) VALUES (?1,?2)")
        .bind(&id)
        .bind(serde_json::to_string(&state).unwrap())
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    let facts = fixture
        .facade
        .load_resource_owner_facts(&id, 1)
        .await
        .unwrap();
    assert_eq!(facts.revision, 7);
    assert_eq!(facts.current_owner, Some(owner.clone()));
    assert_eq!(facts.previous_owner, Some(owner));
    assert_eq!(
        facts.previous_child_metadata.as_deref(),
        Some("{\"version\":1}")
    );

    for corrupt_json in [
        "not json",
        r#"{"revision":7,"resourceOwners":{"1":{"recipientLifecycle":"bad"}}}"#,
        r#"{"revision":7,"resourceOwners":[]}"#,
    ] {
        sqlx::query("UPDATE session_work_state SET state_json=?2 WHERE session_id=?1")
            .bind(&id)
            .bind(corrupt_json)
            .execute(fixture.facade.local_pool())
            .await
            .unwrap();
        assert!(fixture
            .facade
            .load_resource_owner_facts(&id, 0)
            .await
            .is_err());
    }
}

async fn pending(fixture: &Fixture, session: &str) {
    sqlx::query("INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json) VALUES (?1,?1,'digest','undecodable command')")
        .bind(session)
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_work_root_and_descendants_block_until_reconciled() {
    for owner in ["root", "child", "grandchild"] {
        let fixture = Fixture::new().await;
        fixture.create("root").await;
        fixture.child("child", "root").await;
        let mut grandchild = fixture.child_snapshot("grandchild", "child").await;
        grandchild.root_id = "root".into();
        fixture.facade.save_child(&grandchild).await.unwrap();
        fixture.create("unrelated").await;
        pending(&fixture, "unrelated").await;
        fixture
            .facade
            .append_history(&"child".into(), &[payload("allowed")])
            .await
            .unwrap();
        pending(&fixture, owner).await;
        for target in ["root", "child", "grandchild"] {
            let error = fixture
                .facade
                .append_history(&target.into(), &[payload("blocked")])
                .await
                .unwrap_err();
            assert!(error.is_persistence_uncertain());
        }
        assert_eq!(fixture.count_messages("root").await, 0);
        assert_eq!(fixture.count_messages("child").await, 1);
        assert_eq!(fixture.count_messages("grandchild").await, 0);
        sqlx::query("UPDATE session_work_commands SET reconciled=1 WHERE session_id=?1")
            .bind(owner)
            .execute(fixture.facade.local_pool())
            .await
            .unwrap();
        fixture
            .facade
            .append_history(&"root".into(), &[payload("reconciled")])
            .await
            .unwrap();
        assert_eq!(fixture.count_messages("root").await, 1);
    }
}

#[tokio::test]
async fn pending_work_gate_skips_large_undecodable_state_but_keeps_unknown_blocked() {
    let fixture = Fixture::new().await;
    fixture.create("root").await;
    let root = "root".to_owned();
    sqlx::query("INSERT INTO session_work_state(session_id,state_json) VALUES (?1,?2)")
        .bind(&root)
        .bind("undecodable state".repeat(100_000))
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    assert!(fixture
        .facade
        .load_session_work(&WorkQuery {
            session_id: root.clone(),
            limit: 1
        })
        .await
        .is_err());
    fixture
        .facade
        .append_history(&root, &[payload("no state read")])
        .await
        .unwrap();
    let unknown: SessionResourceResult<()> = fixture
        .facade
        .gate
        .with_mutation(&root, || async { Err(commit_failure(Some(root.clone()))) })
        .await;
    assert!(unknown.unwrap_err().is_persistence_uncertain());
    assert!(fixture
        .facade
        .load_resource_owner_facts(&root, 0)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(!fixture
        .facade
        .gate
        .data()
        .has_pending_work_mutations(&root)
        .await
        .unwrap());
    assert!(fixture
        .facade
        .append_history(&root, &[payload("blocked")])
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(fixture.count_messages("root").await, 1);
}

#[tokio::test]
async fn pending_work_database_error_blocks_mutation() {
    let fixture = Fixture::new().await;
    fixture.create("root").await;
    sqlx::query("DROP TABLE session_work_commands")
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    assert!(fixture
        .facade
        .append_history(&"root".into(), &[payload("blocked")])
        .await
        .is_err());
    assert_eq!(fixture.count_messages("root").await, 0);
}

#[tokio::test]
async fn pending_work_missing_root_keeps_registration_allowed() {
    let fixture = Fixture::new().await;
    fixture.create("new-root").await;
    assert_eq!(fixture.count_threads("new-root").await, 1);
}

#[tokio::test]
async fn availability_sqlite_waits_for_root_and_preserves_uncertain_read_rules() {
    let fixture = Fixture::new().await;
    fixture.create("root").await;
    fixture.child("child", "root").await;
    let root = "root".to_owned();
    let child = "child".to_owned();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let writer = {
        let facade = fixture.facade.clone();
        let root = root.clone();
        tokio::spawn(async move {
            facade
                .gate
                .with_exclusive(&root, || async {
                    entered_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(())
                })
                .await
        })
    };
    entered_rx.await.unwrap();
    let read = fixture.facade.load_work_availability(&child);
    tokio::pin!(read);
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut read)
        .await
        .is_err());
    release_tx.send(()).unwrap();
    writer.await.unwrap().unwrap();
    read.await.unwrap();
    let unknown: SessionResourceResult<()> = fixture
        .facade
        .gate
        .with_mutation(&root, || async { Err(commit_failure(Some(root.clone()))) })
        .await;
    assert!(unknown.unwrap_err().is_persistence_uncertain());
    for id in [&root, &child] {
        assert!(fixture
            .facade
            .load_work_revision(id)
            .await
            .unwrap_err()
            .is_persistence_uncertain());
        assert!(fixture
            .facade
            .load_work_availability(id)
            .await
            .unwrap_err()
            .is_persistence_uncertain());
        assert!(fixture
            .facade
            .load_session_work(&WorkQuery {
                session_id: id.clone(),
                limit: 1
            })
            .await
            .unwrap_err()
            .is_persistence_uncertain());
    }
}

#[tokio::test]
async fn work_revision_reads_only_the_schema17_revision() {
    let fixture = Fixture::new().await;
    fixture.create("root").await;
    let root = "root".to_owned();
    assert_eq!(fixture.facade.load_work_revision(&root).await.unwrap(), 0);
    sqlx::query(
        "INSERT INTO session_work_state(session_id,state_json) VALUES (?1,'{\"revision\":7}')",
    )
    .bind(&root)
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();
    assert_eq!(fixture.facade.load_work_revision(&root).await.unwrap(), 7);
    assert!(fixture
        .facade
        .load_work_revision(&"missing".into())
        .await
        .is_err());
}

#[tokio::test]
async fn availability_sqlite_legacy_history_without_ledger_keeps_pending_hint() {
    let fixture = Fixture::new().await;
    fixture.create("root").await;
    let root = "root".to_owned();
    fixture
        .facade
        .append_history(&root, &[payload("legacy")])
        .await
        .unwrap();
    sqlx::query("DELETE FROM session_work_state WHERE session_id=?1")
        .bind(&root)
        .execute(fixture.facade.local_pool())
        .await
        .unwrap();
    let narrow = fixture.facade.load_work_availability(&root).await.unwrap();
    assert!(narrow.is_available(1, None));
    assert!(!narrow.is_available(1, Some(0)));
    assert!(fixture
        .facade
        .load_session_work(&WorkQuery {
            session_id: root,
            limit: 1
        })
        .await
        .unwrap()
        .has_pending_current_work());
}
