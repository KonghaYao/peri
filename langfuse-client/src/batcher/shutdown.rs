//! Retain the single worker handle until its join result has been observed.

use tokio::task::JoinHandle;

use super::failure::ShutdownSnapshot;
use crate::LangfuseError;

pub(super) enum WorkerOwner {
    Running(JoinHandle<ShutdownSnapshot>),
    Joined(Result<ShutdownSnapshot, WorkerFailure>),
}

#[derive(Clone, Copy)]
pub(super) struct WorkerFailure {
    cancelled: bool,
}

impl WorkerOwner {
    pub(super) async fn join(&mut self) -> Result<(), LangfuseError> {
        if let Self::Running(handle) = self {
            // Borrow, never take: cancelling this future leaves the handle in
            // the owner for another caller to join. No await after completion
            // until the immutable terminal outcome is installed.
            let outcome = handle.await.map_err(|error| WorkerFailure {
                cancelled: error.is_cancelled(),
            });
            *self = Self::Joined(outcome);
        }
        match self {
            Self::Joined(Ok(snapshot)) => snapshot.result(),
            Self::Joined(Err(failure)) => Err(LangfuseError::WorkerJoinFailed {
                cancelled: failure.cancelled,
            }),
            Self::Running(_) => unreachable!("worker join must publish a terminal outcome"),
        }
    }
}
