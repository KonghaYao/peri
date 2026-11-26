use super::{
    Arc, BaseMessage, MessageFlags, PersistedPayload, PgTestTransport, SessionResourceErrorKind,
    PAGE_SESSION,
};
use crate::sessions::data::SessionDataPort;

use peri_acp_types::context_archive::{
    ArchiveChunkBatch, ArchiveChunkQuery, ArchiveMetadataQuery, ARCHIVE_CHUNKS_PER_BATCH,
    ARCHIVE_CHUNK_BYTES,
};
use peri_acp_types::projection::MessageProjectionDirective;

#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn archive_remote_exact_chunks_and_scoped_catalog_after_exclusion() {
    let history = vec![PersistedPayload::Message(BaseMessage::ai(
        "original code: \"中文\"\n".repeat(1000),
    ))];
    let data = super::remote_with_history(&history).await;
    let id = PAGE_SESSION.to_owned();
    let catalog = peri_acp_types::context_archive::build_catalog(
        id.clone(),
        "closed-group".into(),
        history[0].id(),
        &history,
        "historical data".into(),
    )
    .unwrap();
    data.apply_message_projections(
        &id,
        &[(
            history[0].id(),
            MessageFlags {
                excluded: true,
                truncated: true,
                projection: Some(MessageProjectionDirective {
                    archive_catalog: Some(catalog.clone()),
                    ..Default::default()
                }),
            },
        )],
    )
    .await
    .unwrap();
    let page = data
        .read_archive_metadata(
            &id,
            &ArchiveMetadataQuery {
                anchor: Some(history[0].id()),
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items[0]
            .projection
            .as_ref()
            .unwrap()
            .archive_catalog
            .as_ref()
            .unwrap(),
        &catalog
    );
    let expected = peri_acp_types::store::serialize_persisted_payload(&history[0]).unwrap();
    let mut recovered = Vec::new();
    let chunk_offsets = (0..expected.len()).step_by(ARCHIVE_CHUNK_BYTES);
    for offset in chunk_offsets {
        let chunk = data
            .read_archive_chunk(
                &id,
                &ArchiveChunkQuery {
                    message_id: history[0].id(),
                    committed_tail: page.committed_tail,
                    offset: offset as u64,
                    max_bytes: ARCHIVE_CHUNK_BYTES as u32,
                },
            )
            .await
            .unwrap();
        assert!(chunk.bytes.len() <= ARCHIVE_CHUNK_BYTES);
        assert_eq!(chunk.total_bytes, expected.len() as u64);
        recovered.extend(chunk.bytes);
    }
    assert_eq!(recovered, expected.as_bytes());
    assert!(data
        .read_archive_chunk(
            &"foreign".into(),
            &ArchiveChunkQuery {
                message_id: history[0].id(),
                committed_tail: page.committed_tail,
                offset: 0,
                max_bytes: 1
            }
        )
        .await
        .is_err());
    data.rewind_history(
        &id,
        peri_acp_types::session_resources::RewindBoundary::RemoveFrom(history[0].id()),
        &[],
    )
    .await
    .unwrap();
    assert!(data
        .read_archive_metadata(
            &id,
            &ArchiveMetadataQuery {
                anchor: Some(history[0].id()),
                limit: 1,
                ..Default::default()
            }
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn archive_remote_reads_32_chunks_in_one_bounded_transaction() {
    use std::sync::atomic::Ordering;
    let pool = crate::sessions::remote::postgres_fixture::test_pool().await;
    sqlx::query("CREATE TABLE messages(thread_id TEXT, message_id TEXT, content TEXT, rowid bigint GENERATED ALWAYS AS IDENTITY)")
        .execute(&pool)
        .await
        .unwrap();
    let payload = PersistedPayload::Message(BaseMessage::ai("original".repeat(20000)));
    let original = peri_acp_types::store::serialize_persisted_payload(&payload).unwrap();
    sqlx::query("INSERT INTO messages(thread_id, message_id, content) VALUES ($1, $2, $3)")
        .bind(PAGE_SESSION)
        .bind(payload.id().as_uuid().to_string())
        .bind(&original)
        .execute(&pool)
        .await
        .unwrap();
    let transport = Arc::new(PgTestTransport::for_test(pool));
    let data = super::remote_data(&transport);
    let queries = (0..ARCHIVE_CHUNKS_PER_BATCH)
        .map(|index| ArchiveChunkQuery {
            message_id: payload.id(),
            committed_tail: payload.id(),
            offset: (index * ARCHIVE_CHUNK_BYTES) as u64,
            max_bytes: ARCHIVE_CHUNK_BYTES as u32,
        })
        .collect();
    let batch = ArchiveChunkBatch::try_new(queries).unwrap();
    let chunks = data
        .read_archive_chunks(&PAGE_SESSION.into(), &batch)
        .await
        .unwrap();
    assert_eq!(
        transport.read_batches.load(Ordering::SeqCst),
        1,
        "32个4KiB块必须共用一次远端请求"
    );
    assert_eq!(chunks.len(), ARCHIVE_CHUNKS_PER_BATCH);
    let bytes = chunks
        .into_iter()
        .flat_map(|chunk| {
            assert_eq!(chunk.total_bytes, original.len() as u64);
            chunk.bytes
        })
        .collect::<Vec<_>>();
    assert_eq!(
        bytes,
        original.as_bytes()[..ARCHIVE_CHUNKS_PER_BATCH * ARCHIVE_CHUNK_BYTES]
    );
    let error = data
        .read_archive_chunks(&"foreign".into(), &batch)
        .await
        .unwrap_err();
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::NotFound),
        "批量读取仍须校验会话和尾界: {error}"
    );
}

#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn inherited_grant_is_scoped_to_source_anchor_range_and_committed_tail() {
    use peri_acp_types::context_archive::ArchiveRoot;
    use peri_acp_types::messages::MessageId;

    let pool = crate::sessions::remote::postgres_fixture::test_pool().await;
    sqlx::raw_sql("CREATE TABLE threads(id text primary key, inherited_context text);
        CREATE TABLE messages(thread_id text, message_id text, rowid bigint GENERATED ALWAYS AS IDENTITY)")
        .execute(&pool).await.unwrap();
    let [first, inside, tail, after] = std::array::from_fn(|_| MessageId::new());
    let root = ArchiveRoot {
        source_thread_id: "source".into(),
        first_anchor: first,
        last_anchor: after,
        committed_tail: tail,
    };
    for id in [first, inside, tail, after] {
        sqlx::query("INSERT INTO messages(thread_id, message_id) VALUES ('source', $1)")
            .bind(id.as_uuid().to_string())
            .execute(&pool)
            .await
            .unwrap();
    }
    let inherited = serde_json::json!({
        "flags": {
            first.as_uuid().to_string(): { "projection": { "archive_roots": [root] } }
        }
    });
    sqlx::query("INSERT INTO threads VALUES ('child',$1)")
        .bind(inherited.to_string())
        .execute(&pool)
        .await
        .unwrap();
    let data = super::remote_data(&Arc::new(PgTestTransport::for_test(pool)));
    let child = "child".to_owned();
    let source = "source".to_owned();
    assert_eq!(
        data.inherited_archive_root(&child, &source, inside)
            .await
            .unwrap(),
        root
    );
    assert!(data
        .inherited_archive_root(&child, &source, after)
        .await
        .is_err());
    assert!(data
        .inherited_archive_root(&child, &"other".into(), inside)
        .await
        .is_err());
}
