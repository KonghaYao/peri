use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use peri_acp_types::messages::{ContentBlock, ImageSource, MessageContent};
use peri_acp_types::session::{
    UserInput, UserInputQueueReceipt, UserInputQueueSnapshot, UserInputState,
};
use ratatui_kit::prelude::Atom;
use tokio::sync::mpsc::UnboundedSender;

use super::atoms::{self, PendingAttachment};
use super::steer_queue::{SteerItemState, SteerQueueAction, SteerQueueItem};

pub(crate) static STEERS: Atom<SteerState> = Atom::new(SteerState::default);
pub(crate) static STEER_TX: OnceLock<UnboundedSender<SteerCommand>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) enum SteerCommandKind {
    Refresh,
    Enqueue(UserInput),
    Dispatch(Vec<String>),
    TakeBack { id: String, restore_draft: bool },
}

#[derive(Clone, Debug)]
pub(crate) struct SteerCommand {
    pub(crate) session_id: String,
    pub(crate) epoch: u64,
    pub(crate) command_id: String,
    pub(crate) generation: Option<String>,
    pub(crate) kind: SteerCommandKind,
}

#[derive(Clone, Debug, Default)]
struct SessionSteers {
    epoch: u64,
    snapshot: Option<UserInputQueueSnapshot>,
    pending: Vec<SteerCommand>,
    recovered: Vec<RecoveredInput>,
    delivered: HashSet<String>,
}

#[derive(Clone, Debug)]
struct RecoveredInput {
    epoch: u64,
    input: UserInput,
    preserve_if_occupied: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SteerState {
    pub(crate) enabled: bool,
    sessions: HashMap<String, SessionSteers>,
}

impl SteerState {
    pub(crate) fn reset_session(&mut self, session_id: &str, epoch: u64) {
        let session = self.sessions.entry(session_id.to_owned()).or_default();
        session.epoch = epoch;
        session.snapshot = None;
        for recovered in &mut session.recovered {
            recovered.epoch = epoch;
        }
        // 已确认取回原稿归属会话，可随重载恢复；未确认命令保留旧 epoch，待受信快照确认实例后再恢复。
    }

    pub(crate) fn snapshot(&self, session_id: &str, epoch: u64) -> Option<&UserInputQueueSnapshot> {
        self.sessions
            .get(session_id)
            .filter(|session| session.epoch == epoch)?
            .snapshot
            .as_ref()
    }

    pub(crate) fn accept_snapshot(
        &mut self,
        snapshot: UserInputQueueSnapshot,
        epoch: u64,
        establish: bool,
    ) -> bool {
        let session = self
            .sessions
            .entry(snapshot.session_id.clone())
            .or_default();
        if session.epoch != epoch {
            return false;
        }
        match &session.snapshot {
            Some(previous) if previous.generation != snapshot.generation => return false,
            Some(previous) if previous.revision > snapshot.revision => return false,
            None if !establish => return false,
            _ => {}
        }
        session.snapshot = Some(snapshot);
        true
    }

    pub(crate) fn begin(&mut self, command: SteerCommand) {
        self.sessions
            .entry(command.session_id.clone())
            .or_insert_with(|| SessionSteers {
                epoch: command.epoch,
                ..SessionSteers::default()
            })
            .pending
            .push(command);
    }

    pub(crate) fn pending_command(
        &self,
        session_id: &str,
        command_id: &str,
    ) -> Option<&SteerCommand> {
        self.sessions
            .get(session_id)?
            .pending
            .iter()
            .find(|command| command.command_id == command_id)
    }

    pub(crate) fn bind_command_generation(&mut self, command: &SteerCommand) {
        if let Some(session) = self.sessions.get_mut(&command.session_id)
            && let Some(pending) = session
                .pending
                .iter_mut()
                .find(|pending| pending.command_id == command.command_id)
        {
            pending.generation.clone_from(&command.generation);
        }
    }

    pub(crate) fn resume_pending(&mut self, session_id: &str, epoch: u64) -> Vec<SteerCommand> {
        let Some(session) = self
            .sessions
            .get_mut(session_id)
            .filter(|session| session.epoch == epoch)
        else {
            return Vec::new();
        };
        let Some(snapshot) = &session.snapshot else {
            return Vec::new();
        };
        session
            .pending
            .iter_mut()
            .filter_map(|command| {
                if command.epoch == epoch || command.generation.is_none() {
                    return None;
                }
                // 旧实例结果仍展示以保留原稿；只有相同实例可重试原命令核对回执。
                command.epoch = epoch;
                (command.generation.as_deref() == Some(snapshot.generation.as_str()))
                    .then(|| command.clone())
            })
            .collect()
    }

    pub(crate) fn settle(&mut self, command: &SteerCommand, receipt: UserInputQueueReceipt) {
        self.accept_snapshot(receipt.snapshot, command.epoch, true);
        let session = self.sessions.entry(command.session_id.clone()).or_default();
        session
            .pending
            .retain(|pending| pending.command_id != command.command_id);
        if let Some(input) = receipt.taken_back
            && matches!(
                command.kind,
                SteerCommandKind::TakeBack {
                    restore_draft: true,
                    ..
                }
            )
            && !session
                .recovered
                .iter()
                .any(|recovered| recovered.input.input_id == input.input_id)
        {
            session.recovered.push(RecoveredInput {
                epoch: session.epoch,
                input,
                preserve_if_occupied: false,
            });
        }
    }

    pub(crate) fn rebind_initial(&mut self, command: &SteerCommand, session_id: &str, epoch: u64) {
        if let Some(previous) = self.sessions.get_mut(&command.session_id) {
            previous
                .pending
                .retain(|pending| pending.command_id != command.command_id);
        }
        let mut bound = command.clone();
        bound.session_id = session_id.to_owned();
        bound.epoch = epoch;
        self.begin(bound);
    }

    pub(crate) fn reject(&mut self, command: &SteerCommand, definitely_rejected: bool) {
        let session = self.sessions.entry(command.session_id.clone()).or_default();
        if definitely_rejected {
            session
                .pending
                .retain(|pending| pending.command_id != command.command_id);
            if let SteerCommandKind::Enqueue(input) = &command.kind {
                session.recovered.push(RecoveredInput {
                    epoch: session.epoch,
                    input: input.clone(),
                    preserve_if_occupied: true,
                });
            }
        }
        // 结果不明确时仍保留原请求和输入身份；不改走旧 prompt 或重新生成 ID。
    }

    pub(crate) fn claim_delivery(&mut self, session_id: &str, input_id: &str) -> bool {
        let session = self.sessions.entry(session_id.to_owned()).or_default();
        let inserted = session.delivered.insert(input_id.to_owned());
        session.pending.retain(|command| {
            !matches!(&command.kind, SteerCommandKind::Enqueue(input) if input.input_id == input_id)
        });
        if let Some(snapshot) = &mut session.snapshot {
            snapshot.items.retain(|item| item.input_id != input_id);
        }
        inserted
    }

    pub(crate) fn recover(
        &mut self,
        session_id: &str,
        epoch: u64,
        draft_is_empty: bool,
    ) -> Option<UserInput> {
        let session = self
            .sessions
            .get_mut(session_id)
            .filter(|session| session.epoch == epoch)?;
        if !draft_is_empty {
            // 撤回只在空输入框恢复，不能在用户清空新稿后再次自动填入。
            // 明确提交失败的原稿仍保留，避免将撤回规则应用到未发送失败。
            session
                .recovered
                .retain(|recovered| recovered.epoch != epoch || recovered.preserve_if_occupied);
            return None;
        }
        let index = session
            .recovered
            .iter()
            .position(|recovered| recovered.epoch == epoch)?;
        Some(session.recovered.remove(index).input)
    }

    pub(crate) fn pending_recovery_ids(&self, session_id: &str) -> Vec<String> {
        let Some(session) = self.sessions.get(session_id) else {
            return Vec::new();
        };
        session
            .recovered
            .iter()
            .filter(|recovered| recovered.epoch == session.epoch)
            .map(|recovered| recovered.input.input_id.clone())
            .collect()
    }

    pub(crate) fn rows(&self, session_id: &str) -> Vec<SteerQueueItem> {
        let Some(session) = self.sessions.get(session_id) else {
            return Vec::new();
        };
        let mut rows: Vec<_> = session
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| &snapshot.items)
            .filter(|item| !session.delivered.contains(&item.input_id))
            .map(|item| SteerQueueItem {
                id: item.input_id.clone(),
                text: item.original_draft.clone(),
                state: if item.state == UserInputState::Queued {
                    SteerItemState::Queued
                } else {
                    SteerItemState::Dispatching
                },
            })
            .collect();
        for command in session
            .pending
            .iter()
            .filter(|command| command.epoch == session.epoch)
        {
            match &command.kind {
                SteerCommandKind::Enqueue(input) => {
                    if !rows.iter().any(|row| row.id == input.input_id)
                        && !session.delivered.contains(&input.input_id)
                    {
                        rows.push(SteerQueueItem {
                            id: input.input_id.clone(),
                            text: input.original_draft.clone(),
                            state: SteerItemState::Submitting,
                        });
                    }
                }
                SteerCommandKind::Dispatch(ids) => {
                    for row in &mut rows {
                        if ids.contains(&row.id) {
                            row.state = SteerItemState::Dispatching;
                        }
                    }
                }
                SteerCommandKind::TakeBack { id, .. } => {
                    if let Some(row) = rows.iter_mut().find(|row| row.id == *id) {
                        row.state = SteerItemState::Withdrawing;
                    }
                }
                SteerCommandKind::Refresh => {}
            }
        }
        for recovered in session
            .recovered
            .iter()
            .filter(|recovered| recovered.epoch == session.epoch)
        {
            rows.push(SteerQueueItem {
                id: recovered.input.input_id.clone(),
                text: recovered.input.original_draft.clone(),
                state: SteerItemState::Withdrawing,
            });
        }
        rows
    }
}

pub(crate) fn is_enabled() -> bool {
    STEERS.state().read().enabled
}

pub(crate) fn session_boundary(session_id: &str, epoch: u64) {
    STEERS.state().write().reset_session(session_id, epoch);
}

pub(crate) fn establish_session_snapshot(snapshot: UserInputQueueSnapshot) {
    if atoms::ACTIVE_SESSION_ID.state().read().as_str() != snapshot.session_id {
        return;
    }
    let epoch = atoms::BRIDGE_RESET_COUNTER.get();
    let retries = {
        let atom = STEERS.state();
        let mut state = atom.write();
        let session_id = snapshot.session_id.clone();
        if !state.accept_snapshot(snapshot, epoch, true) {
            return;
        }
        state.resume_pending(&session_id, epoch)
    };
    for command in retries {
        let _ = send(command);
    }
}

pub(crate) fn enqueue(
    original_draft: String,
    attachments: Vec<PendingAttachment>,
) -> Result<(), String> {
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let epoch = atoms::BRIDGE_RESET_COUNTER.get();
    let input = UserInput {
        input_id: uuid::Uuid::now_v7().to_string(),
        content: content_for_draft(&original_draft, &attachments),
        original_draft,
    };
    let command = SteerCommand {
        session_id,
        epoch,
        command_id: uuid::Uuid::now_v7().to_string(),
        generation: None,
        kind: SteerCommandKind::Enqueue(input),
    };
    STEERS.state().write().begin(command.clone());
    if let Err(error) = send(command.clone()) {
        STEERS.state().write().reject(&command, true);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn act(action: SteerQueueAction, draft_is_empty: bool) {
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let epoch = atoms::BRIDGE_RESET_COUNTER.get();
    let atom = STEERS.state();
    let mut state = atom.write();
    let rows = state.rows(&session_id);
    let queued = |id: &str| {
        rows.iter()
            .any(|row| row.id == id && row.state == SteerItemState::Queued)
    };
    let kind = match action {
        SteerQueueAction::Dispatch { ids } => {
            let ids: Vec<_> = ids.into_iter().filter(|id| queued(id)).collect();
            if ids.is_empty() {
                return;
            }
            SteerCommandKind::Dispatch(ids)
        }
        SteerQueueAction::TakeBack { id } if queued(&id) => SteerCommandKind::TakeBack {
            id,
            restore_draft: draft_is_empty,
        },
        _ => return,
    };
    let command = SteerCommand {
        session_id: session_id.clone(),
        epoch,
        command_id: uuid::Uuid::now_v7().to_string(),
        generation: state
            .snapshot(&session_id, epoch)
            .map(|snapshot| snapshot.generation.clone()),
        kind,
    };
    state.begin(command.clone());
    drop(state);
    if send(command.clone()).is_err() {
        STEERS.state().write().reject(&command, true);
    }
}

fn send(command: SteerCommand) -> Result<(), String> {
    STEER_TX
        .get()
        .ok_or_else(|| "user input consumer unavailable".to_owned())?
        .send(command)
        .map_err(|_| "user input consumer closed".to_owned())
}

fn content_for_draft(text: &str, attachments: &[PendingAttachment]) -> MessageContent {
    if attachments.is_empty() {
        return MessageContent::text(text);
    }
    let mut blocks = vec![ContentBlock::text(text)];
    blocks.extend(attachments.iter().map(|attachment| {
        ContentBlock::image_base64(
            attachment.media_type.clone(),
            attachment.base64_data.clone(),
        )
    }));
    MessageContent::blocks(blocks)
}

pub(crate) fn attachments_from_content(content: &MessageContent) -> Vec<PendingAttachment> {
    let MessageContent::Blocks(blocks) = content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image {
                source: ImageSource::Base64 { media_type, data },
            } => Some(PendingAttachment {
                label: media_type.clone(),
                media_type: media_type.clone(),
                base64_data: data.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "steer_state_test.rs"]
mod tests;
