use crate::archive_chunk_batch::ArchiveChunkBatch;
use crate::archive_chunk_batch::ARCHIVE_CHUNKS_PER_BATCH;
use crate::context_archive::ArchiveChunkQuery;
use crate::context_archive::ArchiveError;
use crate::context_archive::ARCHIVE_CHUNK_BYTES;
use crate::messages::MessageId;

fn query(tail: MessageId) -> ArchiveChunkQuery {
    ArchiveChunkQuery {
        message_id: MessageId::new(),
        committed_tail: tail,
        offset: 0,
        max_bytes: ARCHIVE_CHUNK_BYTES as u32,
    }
}

#[test]
fn archive_chunk_batch_rejects_unbounded_or_inconsistent_requests() {
    let tail = MessageId::new();
    assert!(ArchiveChunkBatch::try_new(vec![query(tail); ARCHIVE_CHUNKS_PER_BATCH]).is_ok());
    for queries in [
        Vec::new(),
        vec![query(tail); ARCHIVE_CHUNKS_PER_BATCH + 1],
        vec![query(tail), query(MessageId::new())],
        vec![ArchiveChunkQuery {
            max_bytes: 0,
            ..query(tail)
        }],
        vec![ArchiveChunkQuery {
            max_bytes: ARCHIVE_CHUNK_BYTES as u32 + 1,
            ..query(tail)
        }],
        vec![ArchiveChunkQuery {
            offset: i64::MAX as u64,
            ..query(tail)
        }],
    ] {
        assert!(matches!(
            ArchiveChunkBatch::try_new(queries),
            Err(ArchiveError::InvalidRequest(_))
        ));
    }
}
