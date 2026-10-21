use super::*;
use peri_acp_types::{
    context_archive::*, projection::MessageProjectionDirective, store::serialize_persisted_payload,
};

#[tokio::test]
async fn archive_remote_exact_chunks_and_scoped_catalog_after_exclusion() {
    let history = vec![PersistedPayload::Message(BaseMessage::ai(
        "original code: \"中文\"\n".repeat(1000),
    ))];
    let (_directory, data) = remote_with_history(&history).await;
    let id = PAGE_SESSION.to_owned();
    let catalog = build_catalog(
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
    let expected = serialize_persisted_payload(&history[0]).unwrap();
    let mut recovered = Vec::new();
    for offset in (0..expected.len()).step_by(ARCHIVE_CHUNK_BYTES) {
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
async fn archive_remote_reads_32_chunks_in_one_bounded_transaction() {
    use std::sync::atomic::Ordering;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE messages(thread_id TEXT, message_id TEXT, content TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    let payload = PersistedPayload::Message(BaseMessage::ai("original".repeat(20000)));
    let original = serialize_persisted_payload(&payload).unwrap();
    sqlx::query("INSERT INTO messages VALUES (?1, ?2, ?3)")
        .bind(PAGE_SESSION)
        .bind(payload.id().as_uuid().to_string())
        .bind(&original)
        .execute(&pool)
        .await
        .unwrap();
    let transport = Arc::new(SqliteTransport::for_test(pool));
    let data = remote_data(&transport);
    let queries = (0..ARCHIVE_CHUNKS_PER_BATCH)
        .map(|index| ArchiveChunkQuery {
            message_id: payload.id(),
            committed_tail: payload.id(),
            offset: (index * ARCHIVE_CHUNK_BYTES) as u64,
            max_bytes: ARCHIVE_CHUNK_BYTES as u32,
        })
        .collect();
    let batch = ArchiveChunkBatch::new(queries).unwrap();
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
