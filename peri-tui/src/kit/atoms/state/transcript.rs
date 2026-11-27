//! Bridge publication metadata read together with the transcript snapshot.
use super::AtomStatic;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TranscriptPublication {
    pub(crate) generation: u64,
    pub(crate) previous_generation: u64,
    pub(crate) changed_from: usize,
    /// Bridge epochs committed with this snapshot. A local fold can advance the snapshot
    /// generation while inheriting the same committed page and release.
    pub(crate) prepend_epoch: u64,
    pub(crate) structural_epoch: u64,
}

pub(crate) static TRANSCRIPT_PUBLICATION: AtomStatic<TranscriptPublication> =
    AtomStatic::new(TranscriptPublication::default);
