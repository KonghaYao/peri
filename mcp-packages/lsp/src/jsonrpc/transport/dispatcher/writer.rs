//! A single writer owns stdin; caller cancellation never interrupts an admitted frame.

use std::sync::Weak;

use tokio::{
    io::AsyncWrite,
    sync::{mpsc, oneshot},
};

use super::DispatchState;
use crate::{error::LspError, jsonrpc::codec};

// Bounded in frames; producers wait for admission instead of dropping traffic.
pub(super) const FRAME_QUEUE_CAPACITY: usize = 16;

pub(super) struct Frame {
    pub(super) body: String,
    pub(super) ack: oneshot::Sender<Result<(), LspError>>,
}

pub(super) async fn run(
    mut stdin: impl AsyncWrite + Unpin,
    mut frames: mpsc::Receiver<Frame>,
    state: Weak<DispatchState>,
) {
    while let Some(frame) = frames.recv().await {
        match codec::encode_message(frame.body.as_bytes(), &mut stdin).await {
            Ok(()) => {
                // The waiter may have cancelled; the admitted frame is complete.
                let _ = frame.ack.send(Ok(()));
            }
            Err(error) => {
                if let Some(state) = state.upgrade() {
                    state.writer_failed();
                }
                // Preserve the initiating write error; other queued senders see
                // TransportClosed when dropping the receiver settles their acks.
                let _ = frame.ack.send(Err(error));
                return;
            }
        }
    }
}
