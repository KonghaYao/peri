//! inherited 摘要、布局与有界缓存的行为回归。
//!
//! 覆盖 F12a：summary/layout 对同一 blob 一致、位置映射与线性扫描一致、缓存超容量后
//! 旧条目失效并可重新扫描、空/缺席/单 payload 边界，以及非 canonical 信封的一致回退。

use super::inherited_blob::{InheritedFactsRead, InheritedSummaryRead};
use super::inherited_facts::{
    CachedInheritedFacts, InheritedCacheLookup, InheritedFactsCache, InheritedSummary,
    INHERITED_FACTS_CACHE_CAPACITY,
};
use super::*;
use crate::sessions::data::SessionDataPort;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta};
use peri_acp_types::store::{InheritedContext, PersistedPayload, ThreadStore};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use std::sync::Arc;
use tempfile::TempDir;

async fn database() -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    (store, data, directory)
}

fn frozen(marker: &str) -> FrozenSnapshotBytes {
    FrozenSnapshotBytes::new(format!(r#"{{"version":1,"marker":"{marker}"}}"#))
}

fn session(
    id: &str,
    cwd: &str,
    workspace: &ResolvedWorkspace,
    frozen: FrozenSnapshotBytes,
) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-10-19T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: cwd.to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen,
    }
}

async fn new_session(
    store: &SqliteThreadStore,
    data: &SqliteSessionData,
    directory: &TempDir,
    id: &str,
) {
    let workspace = store.resolve_workspace(directory.path()).await.unwrap();
    data.save_new_session(&session(
        id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(id),
    ))
    .await
    .unwrap();
}

async fn session_with_inherited(
    payloads: &[PersistedPayload],
    ancestor: &str,
) -> (SqliteThreadStore, SqliteSessionData, TempDir, String) {
    let (store, data, directory) = database().await;
    let id = "inherited-facts-session".to_owned();
    new_session(&store, &data, &directory, &id).await;
    let context = InheritedContext {
        payloads: payloads.to_vec(),
        flags: std::collections::HashMap::new(),
    }
    .to_json()
    .unwrap();
    set_raw_context(&store, &id, Some(&context)).await;
    sqlx::query("UPDATE threads SET parent_thread_id = ?1 WHERE id = ?2")
        .bind(ancestor)
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    (store, data, directory, id)
}

async fn set_raw_context(store: &SqliteThreadStore, id: &str, value: Option<&str>) {
    sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
        .bind(value)
        .bind(id)
        .execute(&store.database.pool)
        .await
        .unwrap();
}

fn payloads(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("inherited-{index}"))))
        .collect()
}

fn fake_summary(total_bytes: u64) -> InheritedSummary {
    InheritedSummary {
        total_bytes,
        count: 1,
        first_id: Some(MessageId::new()),
        last_id: Some(MessageId::new()),
    }
}

async fn blob_bytes(data: &SqliteSessionData, id: &str) -> u64 {
    let bytes: i64 = sqlx::query_scalar(
        "SELECT length(CAST(inherited_context AS BLOB)) FROM threads WHERE id = ?1",
    )
    .bind(id)
    .fetch_one(&data.database.pool)
    .await
    .unwrap();
    u64::try_from(bytes).unwrap()
}

#[tokio::test]
async fn summary_and_layout_agree_on_same_blob() {
    let payloads = payloads(3);
    let (_store, data, _directory, id) = session_with_inherited(&payloads, "ancestor").await;
    let mut connection = data.database.pool.acquire().await.unwrap();

    let summary_snapshot = match data
        .read_inherited_summary_cached(&mut connection, &id)
        .await
        .unwrap()
    {
        InheritedSummaryRead::Facts {
            snapshot: Some(snapshot),
        } => snapshot,
        _ => panic!("canonical envelope must yield a summary snapshot"),
    };
    let (layout_snapshot, layout) = match data
        .read_inherited_layout_cached(&mut connection, &id)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            snapshot: Some(snapshot),
            layout: Some(layout),
        } => (snapshot, layout),
        _ => panic!("canonical envelope must yield a layout snapshot"),
    };

    assert_eq!(summary_snapshot, layout_snapshot);
    assert_eq!(layout_snapshot.entry_count, payloads.len());
    assert_eq!(layout_snapshot.first_entry, Some(payloads[0].id()));
    assert_eq!(layout_snapshot.last_entry, Some(payloads[2].id()));
    assert_eq!(layout.len(), payloads.len());
    assert_eq!(layout.summary().total_bytes, blob_bytes(&data, &id).await);
}

#[tokio::test]
async fn position_map_matches_linear_scan() {
    let payloads = payloads(4);
    let (_store, data, _directory, id) = session_with_inherited(&payloads, "ancestor").await;
    let mut connection = data.database.pool.acquire().await.unwrap();
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &id)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("canonical envelope must yield a layout"),
    };

    for index in 0..layout.len() {
        let id = layout.id_at(index).expect("index is within the layout");
        let linear = layout
            .payloads
            .iter()
            .position(|payload| payload.id == id)
            .expect("id occurs at its own index");
        assert_eq!(layout.position_of(id), Some(linear));
        assert_eq!(linear, index);
    }
    assert_eq!(layout.position_of(MessageId::new()), None);
    assert_eq!(layout.id_at(layout.len()), None);
}

#[tokio::test]
async fn position_map_keeps_first_occurrence_for_duplicate_ids() {
    let payload = PersistedPayload::Message(BaseMessage::human("duplicate"));
    let payloads = vec![payload.clone(), payload.clone()];
    let (_store, data, _directory, id) = session_with_inherited(&payloads, "ancestor").await;
    let mut connection = data.database.pool.acquire().await.unwrap();
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &id)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("canonical envelope must yield a layout"),
    };

    let linear = layout
        .payloads
        .iter()
        .position(|candidate| candidate.id == payload.id())
        .expect("duplicate id occurs in the layout");
    assert_eq!(linear, 0);
    assert_eq!(layout.position_of(payload.id()), Some(linear));
}

#[tokio::test]
async fn cache_evicts_lru_entries_and_rescan_is_stable() {
    let payloads = payloads(2);
    let (_store, data, _directory, id) = session_with_inherited(&payloads, "ancestor").await;
    let mut connection = data.database.pool.acquire().await.unwrap();
    let before = match data
        .read_inherited_summary_cached(&mut connection, &id)
        .await
        .unwrap()
    {
        InheritedSummaryRead::Facts {
            snapshot: Some(snapshot),
        } => snapshot,
        _ => panic!("canonical envelope must yield a summary snapshot"),
    };
    let total_bytes = blob_bytes(&data, &id).await;

    {
        let mut cache = data.inherited_facts.lock().unwrap();
        for index in 0..INHERITED_FACTS_CACHE_CAPACITY {
            cache.store(
                &format!("filler-{index}"),
                CachedInheritedFacts::Summary(Arc::new(fake_summary(index as u64))),
            );
        }
        assert_eq!(cache.len(), INHERITED_FACTS_CACHE_CAPACITY);
        assert!(matches!(
            cache.lookup(&id, total_bytes),
            InheritedCacheLookup::Miss
        ));
    }

    let after = match data
        .read_inherited_summary_cached(&mut connection, &id)
        .await
        .unwrap()
    {
        InheritedSummaryRead::Facts {
            snapshot: Some(snapshot),
        } => snapshot,
        _ => panic!("rescan must yield the same summary snapshot"),
    };
    assert_eq!(before, after);
}

#[test]
fn cache_evicts_least_recently_used_entry() {
    let mut cache = InheritedFactsCache::with_capacity(2);
    let first = "cache-thread-0".to_owned();
    let second = "cache-thread-1".to_owned();
    let third = "cache-thread-2".to_owned();
    cache.store(
        &first,
        CachedInheritedFacts::Summary(Arc::new(fake_summary(10))),
    );
    cache.store(
        &second,
        CachedInheritedFacts::Summary(Arc::new(fake_summary(20))),
    );
    assert!(matches!(
        cache.lookup(&first, 10),
        InheritedCacheLookup::Summary(_)
    ));
    cache.store(
        &third,
        CachedInheritedFacts::Summary(Arc::new(fake_summary(30))),
    );

    assert!(matches!(
        cache.lookup(&second, 20),
        InheritedCacheLookup::Miss
    ));
    assert!(matches!(
        cache.lookup(&first, 10),
        InheritedCacheLookup::Summary(_)
    ));
    assert!(matches!(
        cache.lookup(&third, 30),
        InheritedCacheLookup::Summary(_)
    ));
    assert_eq!(cache.len(), 2);
}

#[tokio::test]
async fn summary_handles_empty_absent_and_single_payload() {
    let (store, data, directory) = database().await;
    let mut connection = data.database.pool.acquire().await.unwrap();

    let empty_blob = "empty-blob".to_owned();
    new_session(&store, &data, &directory, &empty_blob).await;
    set_raw_context(&store, &empty_blob, Some("")).await;
    assert!(matches!(
        data.read_inherited_summary_cached(&mut connection, &empty_blob)
            .await
            .unwrap(),
        InheritedSummaryRead::Facts { snapshot: None }
    ));

    let absent = "absent-column".to_owned();
    new_session(&store, &data, &directory, &absent).await;
    set_raw_context(&store, &absent, None).await;
    assert!(matches!(
        data.read_inherited_summary_cached(&mut connection, &absent)
            .await
            .unwrap(),
        InheritedSummaryRead::Facts { snapshot: None }
    ));

    let empty_envelope = "empty-envelope".to_owned();
    new_session(&store, &data, &directory, &empty_envelope).await;
    let empty_context = InheritedContext::default().to_json().unwrap();
    set_raw_context(&store, &empty_envelope, Some(&empty_context)).await;
    assert!(matches!(
        data.read_inherited_summary_cached(&mut connection, &empty_envelope)
            .await
            .unwrap(),
        InheritedSummaryRead::Facts { snapshot: None }
    ));

    let single = "single-payload".to_owned();
    new_session(&store, &data, &directory, &single).await;
    let payloads = payloads(1);
    let context = InheritedContext {
        payloads: payloads.clone(),
        flags: std::collections::HashMap::new(),
    }
    .to_json()
    .unwrap();
    set_raw_context(&store, &single, Some(&context)).await;
    let snapshot = match data
        .read_inherited_summary_cached(&mut connection, &single)
        .await
        .unwrap()
    {
        InheritedSummaryRead::Facts {
            snapshot: Some(snapshot),
        } => snapshot,
        _ => panic!("single payload must yield a snapshot"),
    };
    assert_eq!(snapshot.entry_count, 1);
    assert_eq!(snapshot.first_entry, Some(payloads[0].id()));
    assert_eq!(snapshot.last_entry, Some(payloads[0].id()));

    match data
        .read_inherited_layout_cached(&mut connection, &single)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => {
            assert_eq!(layout.len(), 1);
            assert_eq!(layout.position_of(payloads[0].id()), Some(0));
            assert_eq!(layout.id_at(1), None);
        }
        _ => panic!("single payload must yield a layout"),
    }
}

#[tokio::test]
async fn non_canonical_envelope_marks_both_reads_unsupported() {
    let (store, data, directory) = database().await;
    let id = "non-canonical".to_owned();
    new_session(&store, &data, &directory, &id).await;
    set_raw_context(&store, &id, Some(r#"{"version":2,"payloads":[]}"#)).await;
    let mut connection = data.database.pool.acquire().await.unwrap();

    assert!(matches!(
        data.read_inherited_summary_cached(&mut connection, &id)
            .await
            .unwrap(),
        InheritedSummaryRead::Unsupported
    ));
    assert!(matches!(
        data.read_inherited_layout_cached(&mut connection, &id)
            .await
            .unwrap(),
        InheritedFactsRead::Unsupported
    ));
}

#[tokio::test]
async fn bounds_use_summary_without_building_layout() {
    let payloads = payloads(2);
    let (_store, data, _directory, id) = session_with_inherited(&payloads, "ancestor").await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds
        .inherited
        .snapshot()
        .expect("inherited prefix is present");
    assert_eq!(snapshot.first_entry, Some(payloads[0].id()));

    let total_bytes = blob_bytes(&data, &id).await;
    let mut cache = data.inherited_facts.lock().unwrap();
    assert!(matches!(
        cache.lookup(&id, total_bytes),
        InheritedCacheLookup::Summary(_)
    ));
}
