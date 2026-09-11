//! One bounded command queue: admission, oldest-event replacement, and closing
//! share a synchronous commit boundary. Capacity waiters are owned by Tokio.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tokio::sync::{watch, Notify, OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::BatcherCommand;
use crate::{BackpressurePolicy, IngestionEvent, LangfuseError};

pub(super) struct Admission {
    shared: Arc<SharedQueue>,
}

pub(super) struct CommandReceiver {
    shared: Arc<SharedQueue>,
}

struct SharedQueue {
    state: Mutex<QueueState>,
    slots: Arc<Semaphore>,
    ready: Notify,
    closing: watch::Sender<bool>,
}

struct QueueState {
    accepting: bool,
    commands: VecDeque<QueuedCommand>,
}

struct QueuedCommand {
    command: BatcherCommand,
    permit: OwnedSemaphorePermit,
}

pub(super) enum AdmissionOutcome {
    Accepted,
    ReplacedOldest,
}

impl SharedQueue {
    fn close(&self) {
        let mut state = self.state.lock().expect("batch admission poisoned");
        state.accepting = false;
        self.slots.close();
        // Closing cannot require a queue slot or a suspended producer's permit.
        self.closing.send_replace(true);
        self.ready.notify_one();
    }
}

impl Admission {
    pub(super) fn new(capacity: usize) -> (Self, CommandReceiver, watch::Receiver<bool>) {
        let (closing, receiver) = watch::channel(false);
        let shared = Arc::new(SharedQueue {
            state: Mutex::new(QueueState {
                accepting: true,
                commands: VecDeque::new(),
            }),
            slots: Arc::new(Semaphore::new(capacity)),
            ready: Notify::new(),
            closing,
        });
        (
            Self {
                shared: Arc::clone(&shared),
            },
            CommandReceiver { shared },
            receiver,
        )
    }

    pub(super) fn close(&self) {
        self.shared.close();
    }

    pub(super) fn try_add(
        &self,
        event: IngestionEvent,
        policy: BackpressurePolicy,
    ) -> Result<AdmissionOutcome, LangfuseError> {
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        if !state.accepting {
            return Err(LangfuseError::ChannelClosed);
        }
        let (permit, outcome) = match Arc::clone(&self.shared.slots).try_acquire_owned() {
            Ok(permit) => (permit, AdmissionOutcome::Accepted),
            Err(TryAcquireError::Closed) => return Err(LangfuseError::ChannelClosed),
            Err(TryAcquireError::NoPermits) => {
                if policy != BackpressurePolicy::DropOldest {
                    return Err(LangfuseError::QueueFull);
                }
                // A submitted flush protects all preceding commands. A new Add
                // cannot erase its obligations or move ahead of that barrier.
                let eligible = state
                    .commands
                    .iter()
                    .rposition(|queued| matches!(queued.command, BatcherCommand::Flush(_)))
                    .map_or(0, |index| index + 1);
                let Some(index) = (eligible..state.commands.len())
                    .find(|&index| matches!(state.commands[index].command, BatcherCommand::Add(_)))
                else {
                    return Err(LangfuseError::QueueFull);
                };
                let replaced = state
                    .commands
                    .remove(index)
                    .expect("eligible command exists");
                // Reuse the old slot, rather than releasing it to a waiter and
                // then accidentally overcommitting capacity for the new event.
                (replaced.permit, AdmissionOutcome::ReplacedOldest)
            }
        };
        state.commands.push_back(QueuedCommand {
            command: BatcherCommand::Add(event),
            permit,
        });
        self.shared.ready.notify_one();
        Ok(outcome)
    }

    pub(super) async fn send(&self, command: BatcherCommand) -> Result<(), LangfuseError> {
        let permit = Arc::clone(&self.shared.slots)
            .acquire_owned()
            .await
            .map_err(|_| LangfuseError::ChannelClosed)?;
        // No await from the final closed check through commit. Dropping a
        // pending sender releases its permit; closing drains committed work only.
        self.commit(command, permit)
    }

    fn commit(
        &self,
        command: BatcherCommand,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), LangfuseError> {
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        if !state.accepting {
            return Err(LangfuseError::ChannelClosed);
        }
        state.commands.push_back(QueuedCommand { command, permit });
        self.shared.ready.notify_one();
        Ok(())
    }
}

impl CommandReceiver {
    pub(super) fn close(&self) {
        self.shared.close();
    }

    pub(super) fn try_recv(&mut self) -> Option<BatcherCommand> {
        self.shared
            .state
            .lock()
            .expect("batch admission poisoned")
            .commands
            .pop_front()
            .map(|queued| queued.command)
    }

    pub(super) async fn recv(&mut self) -> Option<BatcherCommand> {
        loop {
            let notified = self.shared.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.shared.state.lock().expect("batch admission poisoned");
                if let Some(queued) = state.commands.pop_front() {
                    // The slot is returned before processing/HTTP, just as recv
                    // released the original mpsc command queue slot.
                    return Some(queued.command);
                }
                if !state.accepting {
                    return None;
                }
            }
            notified.await;
        }
    }
}

impl Drop for CommandReceiver {
    fn drop(&mut self) {
        // Match mpsc receiver destruction on worker panic/abort: close blocked
        // producers and drop queued flush acks so waiters observe the join error.
        self.shared.close();
        self.shared
            .state
            .lock()
            .expect("batch admission poisoned")
            .commands
            .clear();
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
