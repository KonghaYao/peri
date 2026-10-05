use super::*;

#[tokio::test]
async fn test_frozen_snapshot_roundtrip_and_legacy_null() {
    let (store, _dir) = make_store().await;
    let id = store
        .create_thread(ThreadMeta::new_at(
            absolute_test_path("tmp"),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    let snapshot = r#"{"version":1,"marker":"sqlite-frozen-prefix"}"#;

    assert!(store.load_frozen_snapshot(&id).await.unwrap().is_none());
    assert!(store
        .store_frozen_snapshot_if_absent(&id, snapshot)
        .await
        .unwrap());
    assert!(
        !store
            .store_frozen_snapshot_if_absent(&id, r#"{"version":2}"#)
            .await
            .unwrap(),
        "frozen snapshot is write-once"
    );

    assert_eq!(
        store.load_frozen_snapshot(&id).await.unwrap().as_deref(),
        Some(snapshot)
    );
}

#[tokio::test]
async fn test_frozen_snapshot_concurrent_backfill_has_one_winner() {
    let (store, _dir) = make_store().await;
    let id = store
        .create_thread(ThreadMeta::new_at(
            absolute_test_path("tmp"),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    let first = r#"{"version":1,"candidate":"first"}"#;
    let second = r#"{"version":1,"candidate":"second"}"#;

    let (first_won, second_won) = tokio::join!(
        store.store_frozen_snapshot_if_absent(&id, first),
        store.store_frozen_snapshot_if_absent(&id, second),
    );
    let first_won = first_won.unwrap();
    let second_won = second_won.unwrap();
    assert_ne!(first_won, second_won, "exactly one backfill must win");
    let stored = store.load_frozen_snapshot(&id).await.unwrap().unwrap();
    assert_eq!(stored, if first_won { first } else { second });
}
