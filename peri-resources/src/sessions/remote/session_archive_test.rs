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
