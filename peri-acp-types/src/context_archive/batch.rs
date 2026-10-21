//! A bounded read request: one stable tail, at most 128 KiB of canonical bytes.
use super::{ArchiveChunkQuery, ArchiveError, ARCHIVE_CHUNK_BYTES};

pub const ARCHIVE_CHUNKS_PER_BATCH: usize = 32;

#[derive(Clone, Debug)]
pub struct ArchiveChunkBatch(Vec<ArchiveChunkQuery>);

impl ArchiveChunkBatch {
    pub fn new(queries: Vec<ArchiveChunkQuery>) -> Result<Self, ArchiveError> {
        let Some(first) = queries.first() else {
            return Err(ArchiveError::InvalidRequest(
                "empty archive chunk batch".into(),
            ));
        };
        if queries.len() > ARCHIVE_CHUNKS_PER_BATCH
            || queries.iter().any(|query| {
                query.committed_tail != first.committed_tail || query.validate().is_err()
            })
        {
            return Err(ArchiveError::InvalidRequest(
                "invalid archive chunk batch bounds".into(),
            ));
        }
        Ok(Self(queries))
    }

    pub fn queries(&self) -> &[ArchiveChunkQuery] {
        &self.0
    }
}

impl ArchiveChunkQuery {
    /// The single bound check for canonical archive byte reads, in both adapters.
    pub fn validate(&self) -> Result<(), ArchiveError> {
        if self.max_bytes == 0
            || self.max_bytes as usize > ARCHIVE_CHUNK_BYTES
            || self.offset > i64::MAX as u64 - ARCHIVE_CHUNK_BYTES as u64
        {
            return Err(ArchiveError::InvalidRequest(
                "invalid archive chunk bounds".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "batch_test.rs"]
mod tests;
