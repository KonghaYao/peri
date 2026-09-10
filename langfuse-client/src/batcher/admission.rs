//! Queue admission and closing share one synchronous commit boundary.

use std::sync::Mutex;

use tokio::sync::{mpsc, watch};

use super::BatcherCommand;
use crate::LangfuseError;

pub(super) struct Admission {
    sender: Mutex<Option<mpsc::Sender<BatcherCommand>>>,
    closing: watch::Sender<bool>,
}

impl Admission {
    pub(super) fn new(sender: mpsc::Sender<BatcherCommand>) -> (Self, watch::Receiver<bool>) {
        let (closing, receiver) = watch::channel(false);
        (
            Self {
                sender: Mutex::new(Some(sender)),
                closing,
            },
            receiver,
        )
    }

    pub(super) fn close(&self) {
        let mut sender = self.sender.lock().expect("batch admission poisoned");
        sender.take();
        // Out of band: a full command queue cannot prevent closing.
        self.closing.send_replace(true);
    }

    pub(super) fn try_send(&self, command: BatcherCommand) -> Result<(), LangfuseError> {
        let sender = self.sender.lock().expect("batch admission poisoned");
        sender
            .as_ref()
            .ok_or(LangfuseError::ChannelClosed)?
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => LangfuseError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => LangfuseError::ChannelClosed,
            })
    }

    pub(super) async fn send(&self, command: BatcherCommand) -> Result<(), LangfuseError> {
        let sender = self
            .sender
            .lock()
            .expect("batch admission poisoned")
            .clone()
            .ok_or(LangfuseError::ChannelClosed)?;
        // Waiting producers own only a reservation. They must recheck admission
        // before committing, and cannot retain a permit across another await.
        let permit = sender
            .reserve_owned()
            .await
            .map_err(|_| LangfuseError::ChannelClosed)?;
        let sender = self.sender.lock().expect("batch admission poisoned");
        if sender.is_none() {
            return Err(LangfuseError::ChannelClosed);
        }
        permit.send(command);
        Ok(())
    }
}
