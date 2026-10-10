//! One bounded command queue: admission, oldest-event replacement, and closing
//! share a synchronous commit boundary. Capacity waiters are owned by Tokio.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tokio::sync::{watch, Notify, OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::budget::event_bytes;
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
    space: Notify,
    max_event_bytes: usize,
    max_queue_bytes: usize,
    closing: watch::Sender<bool>,
}

struct QueueState {
    accepting: bool,
    commands: VecDeque<QueuedCommand>,
    bytes: usize,
}

struct QueuedCommand {
    command: BatcherCommand,
    permit: OwnedSemaphorePermit,
    bytes: usize,
}

pub(super) enum AdmissionOutcome {
    Accepted,
    ReplacedOldest(usize),
}

impl SharedQueue {
    fn close(&self) {
        let mut state = self.state.lock().expect("batch admission poisoned");
        state.accepting = false;
        self.slots.close();
        // Closing cannot require a queue slot or a suspended producer's permit.
        self.closing.send_replace(true);
        self.ready.notify_one();
        self.space.notify_waiters();
    }
}

impl Admission {
    #[cfg(test)]
    pub(super) fn new(capacity: usize) -> (Self, CommandReceiver, watch::Receiver<bool>) {
        let config = crate::BatcherConfig::default();
        Self::with_limits(capacity, config.max_event_bytes, config.max_queue_bytes)
    }

    pub(super) fn with_limits(
        capacity: usize,
        max_event_bytes: usize,
        max_queue_bytes: usize,
    ) -> (Self, CommandReceiver, watch::Receiver<bool>) {
        let (closing, receiver) = watch::channel(false);
        let shared = Arc::new(SharedQueue {
            state: Mutex::new(QueueState {
                accepting: true,
                commands: VecDeque::new(),
                bytes: 0,
            }),
            slots: Arc::new(Semaphore::new(capacity)),
            ready: Notify::new(),
            space: Notify::new(),
            max_event_bytes,
            max_queue_bytes,
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
        if self.shared.slots.is_closed() {
            return Err(LangfuseError::ChannelClosed);
        }
        if policy != BackpressurePolicy::DropOldest && self.shared.slots.available_permits() == 0 {
            return Err(LangfuseError::QueueFull);
        }
        let bytes = event_bytes(&event, self.shared.max_event_bytes);
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        if !state.accepting {
            return Err(LangfuseError::ChannelClosed);
        }
        let bytes = bytes?;
        let mut permit = match Arc::clone(&self.shared.slots).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(TryAcquireError::Closed) => return Err(LangfuseError::ChannelClosed),
            Err(TryAcquireError::NoPermits) => None,
        };
        let eligible = state
            .commands
            .iter()
            .rposition(|queued| matches!(queued.command, BatcherCommand::Flush(_)))
            .map_or(0, |index| index + 1);
        let mut removed = Vec::new();
        let needs_replacement =
            permit.is_none() || bytes > self.shared.max_queue_bytes.saturating_sub(state.bytes);
        if needs_replacement {
            let candidates = &state.commands;
            let reclaimable = candidates
                .iter()
                .skip(eligible)
                .map(|queued| queued.bytes)
                .sum::<usize>();
            if policy != BackpressurePolicy::DropOldest
                || eligible == candidates.len()
                || bytes
                    > self
                        .shared
                        .max_queue_bytes
                        .saturating_sub(state.bytes - reclaimable)
            {
                return Err(LangfuseError::QueueFull);
            }
            while permit.is_none()
                || bytes > self.shared.max_queue_bytes.saturating_sub(state.bytes)
            {
                let retired = state
                    .commands
                    .remove(eligible)
                    .expect("eligible command exists");
                state.bytes -= retired.bytes;
                if permit.is_none() {
                    permit = Some(retired.permit);
                }
                removed.push(retired.command);
            }
        }
        state.bytes += bytes;
        state.commands.push_back(QueuedCommand {
            command: BatcherCommand::Add(event),
            permit: permit.expect("accepted event owns a queue slot"),
            bytes,
        });
        drop(state);
        self.shared.ready.notify_one();
        let count = removed.len();
        self.shared.space.notify_waiters();
        Ok(if count == 0 {
            AdmissionOutcome::Accepted
        } else {
            AdmissionOutcome::ReplacedOldest(count)
        })
    }

    pub(super) async fn send(&self, command: BatcherCommand) -> Result<(), LangfuseError> {
        if self.shared.slots.is_closed() {
            return Err(LangfuseError::ChannelClosed);
        }
        let bytes = self.command_bytes(&command)?;
        let permit = Arc::clone(&self.shared.slots)
            .acquire_owned()
            .await
            .map_err(|_| LangfuseError::ChannelClosed)?;
        loop {
            let space = self.shared.space.notified();
            tokio::pin!(space);
            space.as_mut().enable();
            {
                let mut state = self.shared.state.lock().expect("batch admission poisoned");
                if !state.accepting {
                    return Err(LangfuseError::ChannelClosed);
                }
                if bytes <= self.shared.max_queue_bytes.saturating_sub(state.bytes) {
                    state.bytes += bytes;
                    state.commands.push_back(QueuedCommand {
                        command,
                        permit,
                        bytes,
                    });
                    self.shared.ready.notify_one();
                    return Ok(());
                }
            }
            space.await;
        }
    }

    fn command_bytes(&self, command: &BatcherCommand) -> Result<usize, LangfuseError> {
        match command {
            BatcherCommand::Add(event) => event_bytes(event, self.shared.max_event_bytes),
            BatcherCommand::Flush(_) => Ok(0),
        }
    }

    #[cfg(test)]
    fn commit(
        &self,
        command: BatcherCommand,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), LangfuseError> {
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        if !state.accepting {
            return Err(LangfuseError::ChannelClosed);
        }
        let bytes = self.command_bytes(&command)?;
        if bytes > self.shared.max_queue_bytes.saturating_sub(state.bytes) {
            return Err(LangfuseError::QueueFull);
        }
        state.bytes += bytes;
        state.commands.push_back(QueuedCommand {
            command,
            permit,
            bytes,
        });
        self.shared.ready.notify_one();
        Ok(())
    }
}

impl CommandReceiver {
    pub(super) fn close(&self) {
        self.shared.close();
    }

    pub(super) fn try_recv(&mut self) -> Option<BatcherCommand> {
        let command = {
            let mut state = self.shared.state.lock().expect("batch admission poisoned");
            let queued = state.commands.pop_front()?;
            state.bytes -= queued.bytes;
            queued.command
        };
        self.shared.space.notify_waiters();
        Some(command)
    }

    pub(super) async fn recv(&mut self) -> Option<BatcherCommand> {
        loop {
            let notified = self.shared.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.shared.state.lock().expect("batch admission poisoned");
                if let Some(queued) = state.commands.pop_front() {
                    state.bytes -= queued.bytes;
                    // The slot is returned before processing/HTTP, just as recv
                    // released the original mpsc command queue slot.
                    let command = queued.command;
                    drop(queued.permit);
                    drop(state);
                    self.shared.space.notify_waiters();
                    return Some(command);
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
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        state.commands.clear();
        state.bytes = 0;
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;

#[cfg(test)]
#[path = "byte_budget_test.rs"]
mod byte_budget_tests;
