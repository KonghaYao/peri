use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use fluent_bundle::FluentValue;
use peri_acp_types::messages::{ContentBlock, ImageSource, MessageContent};
use peri_acp_types::session::{
    UserInput, UserInputQueueReceipt, UserInputQueueSnapshot, UserInputState,
};
use ratatui_kit::prelude::Atom;
use tokio::sync::mpsc::UnboundedSender;

use super::atoms::{self, PendingAttachment};
use super::steer_queue::{SteerItemState, SteerQueueAction, SteerQueueItem};
use super::tui_render_unit::UserAttachment;

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
    queued_at: HashMap<String, peri_time::Instant>,
    recovered: Vec<RecoveredInput>,
    delivered: HashSet<String>,
    // 仅影响待发送区展示；正式聊天气泡仍由 Delivered 确认。
    direct_submissions: HashSet<String>,
    // 致命执行失败后结果未知的完整请求：保留原稿、附件与命令身份，等待服务端
    // 终态或实例快照对账；不能换 id 重发，也不能把缺失当作已交付。
    unsettled: Vec<UnsettledInput>,
}

/// 致命结算时输入所处的发送阶段。
///
/// `NotSent` 表示命令尚未进入发送分支（`generation` 仍为空），可安全恢复为
/// 草稿并按用户操作重新提交；`MaybeSent` 表示请求可能已到达服务端，必须
/// 保留原 `input_id` 等待正面终态，禁止换身份重发。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnsettledStage {
    NotSent,
    MaybeSent,
}

#[derive(Clone, Debug)]
struct UnsettledInput {
    input: UserInput,
    command_id: String,
    stage: UnsettledStage,
    epoch: u64,
}

/// 致命执行失败的结算报告：用户可见的失败反馈按两类语义分别统计。
#[derive(Clone, Debug, Default)]
pub(crate) struct FatalSubmissionReport {
    /// 确定未发出的输入，已放回原稿恢复队列。
    pub(crate) not_sent: Vec<UserInput>,
    /// 结果未知的输入，完整请求仍保留在 `unsettled`。
    pub(crate) uncertain: Vec<UserInput>,
}

impl FatalSubmissionReport {
    /// 测试断言用：报告是否未结算任何输入。
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.not_sent.is_empty() && self.uncertain.is_empty()
    }

    /// 失败反馈文案：失败不能通过清空输入来“结束”，必须说明输入去向。
    pub(crate) fn notice(&self) -> Option<String> {
        let count_value = |count: usize| FluentValue::from(count as u64);
        match (self.not_sent.len(), self.uncertain.len()) {
            (0, 0) => None,
            (count, 0) => Some(crate::i18n::tr_args(
                "steer-fatal-not-sent",
                &[("count".into(), count_value(count))],
            )),
            (0, count) => Some(crate::i18n::tr_args(
                "steer-fatal-uncertain",
                &[("count".into(), count_value(count))],
            )),
            (not_sent, uncertain) => Some(crate::i18n::tr_args(
                "steer-fatal-mixed",
                &[
                    ("not_sent".into(), count_value(not_sent)),
                    ("uncertain".into(), count_value(uncertain)),
                ],
            )),
        }
    }
}

impl SessionSteers {
    fn reconcile_direct_submissions(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        for item in &snapshot.items {
            if item.state == UserInputState::Queued
                && !self.pending.iter().any(|command| {
                    matches!(&command.kind, SteerCommandKind::Enqueue(input)
                        if input.input_id == item.input_id)
                })
            {
                self.direct_submissions.remove(&item.input_id);
            }
        }
        // 终态是正面事实：Delivered 表示输入已进入执行，Withdrawn 表示输入确定未执行。
        // 只有快照中缺失 ID 才保持未知，不能据此推断成功。
        let terminal: Vec<(String, UserInputState)> = snapshot
            .terminal_items
            .iter()
            .map(|item| (item.input_id.clone(), item.state))
            .collect();
        for (input_id, state) in &terminal {
            if matches!(state, UserInputState::Delivered | UserInputState::Withdrawn) {
                self.direct_submissions.remove(input_id);
            }
        }
        // 快照 items 出现该 ID，或终态给出 Delivered/Withdrawn，都是服务端正面
        // 事实；此时本地未知账本交给真实投影，不能继续自行推断。
        let settled_ids: HashSet<&str> = snapshot
            .items
            .iter()
            .map(|item| item.input_id.as_str())
            .chain(
                terminal
                    .iter()
                    .filter(|(_, state)| {
                        matches!(state, UserInputState::Delivered | UserInputState::Withdrawn)
                    })
                    .map(|(input_id, _)| input_id.as_str()),
            )
            .collect();
        self.unsettled
            .retain(|entry| !settled_ids.contains(entry.input.input_id.as_str()));
        // 终态已经回答了 Enqueue 命令的结果；丢失的回执不应继续显示 Submitting 或重试。
        let terminal_ids: HashSet<&str> = terminal
            .iter()
            .map(|(input_id, _)| input_id.as_str())
            .collect();
        let settled_commands: Vec<String> = self
            .pending
            .iter()
            .filter(|command| {
                matches!(&command.kind, SteerCommandKind::Enqueue(input)
                    if terminal_ids.contains(input.input_id.as_str()))
            })
            .map(|command| command.command_id.clone())
            .collect();
        self.pending.retain(|command| {
            !matches!(&command.kind, SteerCommandKind::Enqueue(input)
                if terminal_ids.contains(input.input_id.as_str()))
        });
        for command_id in settled_commands {
            self.queued_at.remove(&command_id);
        }
        // Delivered 的气泡只由事件泵按有序交付日志推送；终态快照只清除提交反馈。
        // 这里合成气泡会绕过 assistant 输出所在 FIFO，无法证明插入位置。
    }
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
    pub(crate) fn take_queue_wait(
        &mut self,
        command: &SteerCommand,
    ) -> Option<std::time::Duration> {
        self.sessions
            .get_mut(&command.session_id)?
            .queued_at
            .remove(&command.command_id)
            .map(|queued| queued.elapsed())
    }

    pub(crate) fn direct_submitting(&self, session_id: &str, epoch: u64) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(|session| session.epoch == epoch && !session.direct_submissions.is_empty())
    }

    pub(crate) fn reset_session(&mut self, session_id: &str, epoch: u64) {
        let session = self.sessions.entry(session_id.to_owned()).or_default();
        session.epoch = epoch;
        session.snapshot = None;
        session.queued_at.clear();
        session.direct_submissions.clear();
        for recovered in &mut session.recovered {
            recovered.epoch = epoch;
        }
        // 结果未知的完整请求不随会话重载消失：更新 epoch 后继续可见，
        // 由后续同 generation 快照或终态对账。
        for unsettled in &mut session.unsettled {
            unsettled.epoch = epoch;
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
        session.reconcile_direct_submissions();
        true
    }

    pub(crate) fn begin(&mut self, command: SteerCommand) {
        let session = self
            .sessions
            .entry(command.session_id.clone())
            .or_insert_with(|| SessionSteers {
                epoch: command.epoch,
                ..SessionSteers::default()
            });
        if let SteerCommandKind::Enqueue(input) = &command.kind
            && session.pending.is_empty()
            && session.direct_submissions.is_empty()
            && session
                .snapshot
                .as_ref()
                .map_or(command.session_id.is_empty(), |snapshot| {
                    snapshot.active_request_id.is_none() && snapshot.items.is_empty()
                })
        {
            session.direct_submissions.insert(input.input_id.clone());
        }
        session
            .queued_at
            .entry(command.command_id.clone())
            .or_insert_with(peri_time::monotonic_now);
        session.pending.push(command);
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

    /// 把命令绑定的 generation 写回待结算命令；命令已被致命失败结算或
    /// 会话重置时返回 `false`，调用方不得再发送该请求。
    pub(crate) fn bind_command_generation(&mut self, command: &SteerCommand) -> bool {
        let Some(session) = self.sessions.get_mut(&command.session_id) else {
            return false;
        };
        let Some(pending) = session
            .pending
            .iter_mut()
            .find(|pending| pending.command_id == command.command_id)
        else {
            return false;
        };
        pending.generation.clone_from(&command.generation);
        true
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
        session.queued_at.remove(&command.command_id);
        session
            .pending
            .retain(|pending| pending.command_id != command.command_id);
        session.reconcile_direct_submissions();
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
        let mut direct = false;
        if let Some(previous) = self.sessions.get_mut(&command.session_id) {
            previous.queued_at.remove(&command.command_id);
            if let SteerCommandKind::Enqueue(input) = &command.kind {
                direct = previous.direct_submissions.remove(&input.input_id);
            }
            previous
                .pending
                .retain(|pending| pending.command_id != command.command_id);
        }
        let mut bound = command.clone();
        bound.session_id = session_id.to_owned();
        bound.epoch = epoch;
        self.begin(bound);
        if direct && let SteerCommandKind::Enqueue(input) = &command.kind {
            self.sessions
                .get_mut(session_id)
                .expect("bound session exists")
                .direct_submissions
                .insert(input.input_id.clone());
        }
    }

    pub(crate) fn reject(&mut self, command: &SteerCommand, definitely_rejected: bool) {
        let session = self.sessions.entry(command.session_id.clone()).or_default();
        session.queued_at.remove(&command.command_id);
        let mut settled_unknown = false;
        if let SteerCommandKind::Enqueue(input) = &command.kind {
            // 超时/未知结果必须重新可见；明确拒绝则交给原稿恢复。
            session.direct_submissions.remove(&input.input_id);
            if let Some(index) = session.unsettled.iter().position(|entry| {
                entry.input.input_id == input.input_id && entry.command_id == command.command_id
            }) {
                settled_unknown = true;
                // 已在致命失败中结算为结果未知：只有服务端的明确拒绝才能
                // 降级为“确定未发出”，恢复原稿且只生成一份。
                if definitely_rejected {
                    let mut entry = session.unsettled.remove(index);
                    entry.stage = UnsettledStage::NotSent;
                    session.recovered.push(RecoveredInput {
                        epoch: session.epoch,
                        input: entry.input,
                        preserve_if_occupied: true,
                    });
                }
            }
        }
        if definitely_rejected && !settled_unknown {
            session
                .pending
                .retain(|pending| pending.command_id != command.command_id);
            // 明确拒绝只在尚未生成恢复稿时补一份：致命失败已把 `NotSent`
            // 输入放进 `recovered`，迟到的明确拒绝不得再生成重复草稿。
            if let SteerCommandKind::Enqueue(input) = &command.kind
                && !session
                    .recovered
                    .iter()
                    .any(|recovered| recovered.input.input_id == input.input_id)
            {
                session.recovered.push(RecoveredInput {
                    epoch: session.epoch,
                    input: input.clone(),
                    preserve_if_occupied: true,
                });
            }
        }
        // 结果不明确时仍保留原请求和输入身份；不改走旧 prompt 或重新生成 ID。
    }

    /// 致命执行失败（含交付日志溢出）必须结束本地提交反馈，避免用户停留在
    /// “正在提交…”而看不到显式错误；真正的气泡仍不会被伪造。
    ///
    /// 结算只覆盖当前 epoch 的 Enqueue：
    /// - `generation` 为空表示请求尚未进入发送分支，按确定未发出恢复完整原稿；
    /// - `generation` 已绑定表示结果未知，保留完整请求与 `input_id`，等待
    ///   服务端终态或实例快照对账，不自动重发、不生成气泡。
    ///
    /// 旧 epoch 命令留给 `resume_pending` 对账，不能在这里一并删除。
    pub(crate) fn fail_session_submissions(&mut self, session_id: &str) -> FatalSubmissionReport {
        let mut report = FatalSubmissionReport::default();
        let Some(session) = self.sessions.get_mut(session_id) else {
            return report;
        };
        session.direct_submissions.clear();
        let epoch = session.epoch;
        // 已出现在当前实例快照 items 中的输入已有正面队列投影，不再进入未知账本。
        let projected_ids: HashSet<String> = session
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .map(|item| item.input_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let settled: Vec<SteerCommand> = session
            .pending
            .iter()
            .filter(|command| {
                command.epoch == epoch && matches!(command.kind, SteerCommandKind::Enqueue(_))
            })
            .cloned()
            .collect();
        session.pending.retain(|command| {
            !(command.epoch == epoch && matches!(command.kind, SteerCommandKind::Enqueue(_)))
        });
        for command in settled {
            session.queued_at.remove(&command.command_id);
            let SteerCommandKind::Enqueue(input) = command.kind else {
                continue;
            };
            let stage = if command.generation.is_none() {
                UnsettledStage::NotSent
            } else {
                UnsettledStage::MaybeSent
            };
            let entry = UnsettledInput {
                input,
                command_id: command.command_id,
                stage,
                epoch,
            };
            match entry.stage {
                UnsettledStage::NotSent => {
                    session.recovered.push(RecoveredInput {
                        epoch: session.epoch,
                        input: entry.input.clone(),
                        preserve_if_occupied: true,
                    });
                    report.not_sent.push(entry.input);
                }
                UnsettledStage::MaybeSent => {
                    if projected_ids.contains(&entry.input.input_id) {
                        continue;
                    }
                    if let Some(existing) = session
                        .unsettled
                        .iter_mut()
                        .find(|existing| existing.input.input_id == entry.input.input_id)
                    {
                        existing.stage = entry.stage;
                    } else {
                        report.uncertain.push(entry.input.clone());
                        session.unsettled.push(entry);
                    }
                }
            }
        }
        report
    }

    pub(crate) fn claim_delivery(&mut self, session_id: &str, input_id: &str) -> bool {
        let session = self.sessions.entry(session_id.to_owned()).or_default();
        let inserted = session.delivered.insert(input_id.to_owned());
        session.direct_submissions.remove(input_id);
        session
            .unsettled
            .retain(|entry| entry.input.input_id != input_id);
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
            .filter(|item| {
                matches!(
                    item.state,
                    UserInputState::Queued | UserInputState::Dispatching | UserInputState::Claimed
                )
            })
            .filter(|item| !session.delivered.contains(&item.input_id))
            .filter(|item| !session.direct_submissions.contains(&item.input_id))
            .map(|item| SteerQueueItem {
                id: item.input_id.clone(),
                text: item.original_draft.clone(),
                state: match item.state {
                    UserInputState::Queued => SteerItemState::Queued,
                    UserInputState::Claimed => SteerItemState::Claimed,
                    _ => SteerItemState::Dispatching,
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
                    if let Some(row) = rows.iter_mut().find(|row| row.id == input.input_id) {
                        row.state = SteerItemState::Submitting;
                    } else if !session.delivered.contains(&input.input_id)
                        && !session.direct_submissions.contains(&input.input_id)
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
                            row.state = SteerItemState::Publishing;
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
        for unsettled in session
            .unsettled
            .iter()
            .filter(|entry| entry.epoch == session.epoch)
            .filter(|entry| !session.delivered.contains(&entry.input.input_id))
        {
            if rows.iter().any(|row| row.id == unsettled.input.input_id) {
                continue;
            }
            rows.push(SteerQueueItem {
                id: unsettled.input.input_id.clone(),
                text: unsettled.input.original_draft.clone(),
                state: SteerItemState::Uncertain,
            });
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

    fn action_kind(
        &self,
        session_id: &str,
        epoch: u64,
        action: SteerQueueAction,
        draft_is_empty: bool,
    ) -> Option<SteerCommandKind> {
        let snapshot = self.snapshot(session_id, epoch)?;
        let rows = self.rows(session_id);
        let queued = |id: &str| {
            rows.iter()
                .any(|row| row.id == id && row.state == SteerItemState::Queued)
                && snapshot
                    .items
                    .iter()
                    .any(|item| item.input_id == id && item.state == UserInputState::Queued)
        };
        match action {
            SteerQueueAction::Dispatch { ids } => {
                let ids: Vec<_> = ids.into_iter().filter(|id| queued(id)).collect();
                (!ids.is_empty()).then_some(SteerCommandKind::Dispatch(ids))
            }
            SteerQueueAction::TakeBack { id }
                if rows
                    .iter()
                    .any(|row| row.id == id && row.state.can_take_back())
                    && snapshot.items.iter().any(|item| {
                        item.input_id == id
                            && matches!(
                                item.state,
                                UserInputState::Queued | UserInputState::Dispatching
                            )
                    }) =>
            {
                Some(SteerCommandKind::TakeBack {
                    id,
                    restore_draft: draft_is_empty,
                })
            }
            _ => None,
        }
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
        content: content_with_attachments(&original_draft, &attachments),
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
    let Some(kind) = state.action_kind(&session_id, epoch, action, draft_is_empty) else {
        return;
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

/// 由草稿文本 + 待发送附件构造提交内容：首个 text 块（非空时）+ 逐张 image 块。
///
/// 单一事实源：steer 入队（`enqueue`）与主提交路径（`submit_consumer`）共用，
/// 避免两条提交路径的 content 形状漂移。空文本 + 有附件（图片-only 提交）不
/// 产出空 text 块——provider 会拒收空 text block。
pub(crate) fn content_with_attachments(
    text: &str,
    attachments: &[PendingAttachment],
) -> MessageContent {
    if attachments.is_empty() {
        return MessageContent::text(text);
    }
    let mut blocks = Vec::with_capacity(attachments.len() + 1);
    if !text.is_empty() {
        blocks.push(ContentBlock::text(text));
    }
    blocks.extend(attachments.iter().map(|attachment| {
        ContentBlock::image_base64(
            attachment.media_type.clone(),
            attachment.base64_data.clone(),
        )
    }));
    MessageContent::blocks(blocks)
}

/// 待发送附件 → 展示记录（提交路径）：只取渲染所需的顺序与大小，不带 base64 载荷。
pub(crate) fn attachment_displays(attachments: &[PendingAttachment]) -> Vec<UserAttachment> {
    attachments
        .iter()
        .map(|attachment| UserAttachment::from_base64_payload(&attachment.base64_data))
        .collect()
}

/// 投递内容 → 展示记录（投递路径）：与 [`attachment_displays`] 同源口径——两条
/// 提交路径的图片块由 [`content_with_attachments`] 逐字节复制，大小必然一致。
pub(crate) fn attachment_displays_from_content(content: &MessageContent) -> Vec<UserAttachment> {
    let MessageContent::Blocks(blocks) = content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image {
                source: ImageSource::Base64 { data, .. },
            } => Some(UserAttachment::from_base64_payload(data)),
            _ => None,
        })
        .collect()
}

/// 把已投递内容里的图片块还原为待发送附件（撤回路径用）。
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

#[cfg(test)]
#[path = "steer_state_idle_test.rs"]
mod idle_tests;
