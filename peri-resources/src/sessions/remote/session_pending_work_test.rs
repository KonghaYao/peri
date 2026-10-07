use super::*;

async fn pending(fixture: &Fixture, session: &str) {
    sqlx::query("INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json) VALUES (?1,?1,'digest','undecodable command')")
        .bind(session)
        .execute(&fixture.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_work_remote_root_and_descendants_block_until_reconciled() {
    for owner in ["work-session", "gate-child", "gate-grandchild"] {
        let fixture = Fixture::new().await;
        let adapter = fixture.adapter().await;
        for (child, parent) in [
            ("gate-child", "work-session"),
            ("gate-grandchild", "gate-child"),
        ] {
            sqlx::query("INSERT INTO threads(id,created_at,updated_at,parent_thread_id,workspace_id) SELECT ?1,created_at,updated_at,?2,workspace_id FROM threads WHERE id=?2")
                .bind(child)
                .bind(parent)
                .execute(&fixture.pool)
                .await
                .unwrap();
        }
        pending(&fixture, "unrelated").await;
        let root = "work-session".to_owned();
        assert!(!adapter.has_pending_work_mutations(&root).await.unwrap());
        pending(&fixture, owner).await;
        assert!(adapter.has_pending_work_mutations(&root).await.unwrap());
        sqlx::query("UPDATE session_work_commands SET reconciled=1 WHERE session_id=?1")
            .bind(owner)
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert!(!adapter.has_pending_work_mutations(&root).await.unwrap());
    }
}

#[tokio::test]
async fn workrecords_pending_query_skips_large_undecodable_payload() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let root = "work-session".to_owned();
    sqlx::query("INSERT INTO session_payloads(storage_scope,payload_id,kind,codec,version,byte_length,sha256,bytes,retention_class) VALUES (?1,'irrelevant','evidence','json',1,length(?2),'invalid',?2,'evidence')")
        .bind(&root)
        .bind("undecodable payload".repeat(100_000).into_bytes())
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(!adapter.has_pending_work_mutations(&root).await.unwrap());
    pending(&fixture, &root).await;
    assert!(adapter.has_pending_work_mutations(&root).await.unwrap());
}

#[tokio::test]
async fn pending_work_remote_database_error_is_not_treated_as_empty() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    sqlx::query("DROP TABLE session_work_commands")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(adapter
        .has_pending_work_mutations(&"work-session".into())
        .await
        .is_err());
}
