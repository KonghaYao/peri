//! 执行预留、attempt 生命周期、停止、idle 唤醒与领取标记。

use peri_acp_types::{event_v2::StateEvent, messages::MessageId, session::UserInputState};
use tokio_util::sync::CancellationToken;

use super::state::{matches_id, new_ticket, promote_next, ready_ids};
use super::{
    ActiveRun, InterruptReason, UserInputAttemptOutcome, UserInputMailbox, UserInputRunTicket,
};

impl UserInputMailbox {
    /// 只预留一次执行，不交接消息；宿主取得既有 prompt lock 后再 attach。
    pub fn reserve_run(&self) -> Option<UserInputRunTicket> {
        let mut state = self.state.lock();
        if !state.valid || state.paused || state.active.is_some() || state.ready.is_empty() {
            return None;
        }
        let ticket = new_ticket();
        state.active = Some(ActiveRun {
            ticket: ticket.clone(),
            cancel: None,
            reason: InterruptReason::None,
            outcome: None,
            managed: true,
        });
        Some(ticket)
    }

    /// 使用真实执行 token 绑定预留身份；交接与 Stop 在 owner 内顺序裁决。
    pub async fn attach_attempt(
        &self,
        ticket: &UserInputRunTicket,
        cancel: CancellationToken,
    ) -> bool {
        let ready = {
            let mut state = self.state.lock();
            if !state.valid || state.paused {
                return false;
            }
            let Some(active) = state
                .active
                .as_mut()
                .filter(|active| active.ticket == *ticket && active.cancel.is_none())
            else {
                return false;
            };
            active.cancel = Some(cancel);
            // 预留期间的 dispatch 还没有真正中断一个执行。
            active.reason = InterruptReason::None;
            state.revision += 1;
            ready_ids(&state)
        };
        self.send_ready_ids(ready, true).await;
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
        true
    }

    /// 旧 prompt / 后台 continuation 也注册真实执行，以统一判断运行中准入。
    pub async fn attach_external_attempt(
        &self,
        cancel: CancellationToken,
        resume_pending: bool,
    ) -> Option<UserInputRunTicket> {
        let (ticket, ready) = {
            let mut state = self.state.lock();
            if !state.valid || state.active.is_some() {
                return None;
            }
            let ticket = new_ticket();
            state.active = Some(ActiveRun {
                ticket: ticket.clone(),
                cancel: Some(cancel),
                reason: InterruptReason::None,
                outcome: None,
                managed: false,
            });
            state.revision += 1;
            if resume_pending {
                state.paused = false;
                promote_next(&mut state);
            }
            (ticket, ready_ids(&state))
        };
        self.send_ready_ids(ready, true).await;
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
        Some(ticket)
    }

    /// 当前预留的执行身份。
    ///
    /// 宿主在 attach 之前绑定取消令牌、或核对在途 ticket 时使用；只读快照，
    /// 不改变预留状态。
    pub fn active_run_ticket(&self) -> Option<UserInputRunTicket> {
        self.state
            .lock()
            .active
            .as_ref()
            .map(|active| active.ticket.clone())
    }

    /// 宿主须可靠投递此 Agent 事件后才开始执行，确保客户端 HITL lease 已存在。
    pub fn run_started_event(&self, ticket: &UserInputRunTicket) -> Option<StateEvent> {
        let state = self.state.lock();
        if !state.valid
            || !state
                .active
                .as_ref()
                .is_some_and(|active| active.ticket == *ticket && active.cancel.is_some())
        {
            return None;
        }
        Some(StateEvent::UserInputRunStarted {
            turn_id: self.control_turn,
            agent_id: self.control_agent,
            generation: self.generation.clone(),
            request_id: ticket.id.clone(),
        })
    }

    /// Agent 在 transcript flush 和 forwarder drain 后记录权威终态。
    pub(crate) fn record_attempt_outcome(
        &self,
        ticket: &UserInputRunTicket,
        outcome: UserInputAttemptOutcome,
    ) {
        let mut state = self.state.lock();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.ticket == *ticket)
        {
            active.outcome = Some(outcome);
        }
    }

    /// 当前 attempt 是否被用户暂存区发送打断（steer 而非 Stop）。
    ///
    /// 必须在 `finish_attempt` 之前判定：收尾会移除 active 记录，之后恒为 false。
    pub fn is_steer_interruption(&self, ticket: &UserInputRunTicket) -> bool {
        self.state.lock().active.as_ref().is_some_and(|active| {
            active.ticket == *ticket && active.reason == InterruptReason::Steer
        })
    }

    /// 本 ticket 是否仍持有执行准入；宿主据此判断一次尝试是否已结算。
    pub fn is_active_ticket(&self, ticket: &UserInputRunTicket) -> bool {
        self.state
            .lock()
            .active
            .as_ref()
            .is_some_and(|active| active.ticket == *ticket)
    }

    /// 仅测试：观察 Agent 已记录的权威终态（收尾前有效）。
    #[cfg(test)]
    pub(crate) fn recorded_attempt_outcome(
        &self,
        ticket: &UserInputRunTicket,
    ) -> Option<UserInputAttemptOutcome> {
        self.state
            .lock()
            .active
            .as_ref()
            .filter(|active| active.ticket == *ticket)
            .and_then(|active| active.outcome)
    }

    /// fallback 只处理尚未进入 Agent 的早退；正常终态优先采用 Agent 的记录。
    pub async fn finish_attempt(
        &self,
        ticket: &UserInputRunTicket,
        fallback: UserInputAttemptOutcome,
    ) {
        let (reclaim, promote) = {
            let mut state = self.state.lock();
            if !state
                .active
                .as_ref()
                .is_some_and(|active| active.ticket == *ticket)
            {
                return;
            }
            let active = state.active.take().expect("已核对执行身份");
            state.revision += 1;
            state.suspended = false;
            let outcome = active.outcome.unwrap_or(fallback);
            if !state.valid {
                return;
            }
            let reclaim = outcome == UserInputAttemptOutcome::Failed
                || (outcome == UserInputAttemptOutcome::Interrupted
                    && active.reason == InterruptReason::None)
                || state.paused;
            let promote = !reclaim
                && active.reason == InterruptReason::None
                && outcome == UserInputAttemptOutcome::Completed;
            if reclaim {
                state.paused = true;
            }
            (reclaim, promote)
        };
        if reclaim {
            self.reclaim().await;
        } else if promote {
            // 已接受的立即发送优先；普通待办每次只释放队首一条。
            // 这里只做运行期 promotion；真实 `send_inputs` 留给下一次 attach。
            let mut state = self.state.lock();
            promote_next(&mut state);
        }
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }

    /// 宿主任务在开始或执行途中被丢弃时，保留确认尚未被 Receive 领取的输入。
    ///
    /// 只结算仍属于本 ticket 的预留：该预留若已被后续执行接管，再复位会把
    /// 下一条输入一起暂停。Drop 路径不能 await，只能做保守的运行期复位；
    /// MQ 行仍在，下一次 `hydrate`/`reclaim` 会按持久事实核对。
    pub fn fail_reserved(&self, ticket: &UserInputRunTicket) {
        let (cancel, reset_ids) = {
            let mut state = self.state.lock();
            let cancel = {
                let Some(active) = state
                    .active
                    .as_ref()
                    .filter(|active| active.ticket == *ticket)
                else {
                    return;
                };
                active.cancel.clone()
            };
            state.active = None;
            state.revision += 1;
            state.paused = true;
            state.suspended = false;
            for record in &mut state.records {
                if record.state == UserInputState::Dispatching {
                    record.state = UserInputState::Queued;
                    record.handed_off = false;
                }
            }
            state.ready.clear();
            let reset_ids: Vec<MessageId> = state
                .records
                .iter()
                .filter(|record| record.state == UserInputState::Queued && !record.handed_off)
                .filter_map(|record| {
                    uuid::Uuid::parse_str(&record.input_id)
                        .ok()
                        .map(MessageId::from)
                })
                .collect();
            (cancel, reset_ids)
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.inbox.queue().withdraw_send_inputs(&reset_ids);
    }

    pub async fn stop(&self) {
        self.stop_matching(None).await;
    }

    /// 携带服务端执行身份的 Stop；迟到请求不得取消后来的执行。
    pub async fn stop_attempt(&self, request_id: &str, generation: &str) -> bool {
        self.stop_matching(Some((request_id, generation))).await
    }

    async fn stop_matching(&self, target: Option<(&str, &str)>) -> bool {
        let cancel = {
            let mut state = self.state.lock();
            if let Some((request_id, generation)) = target {
                if generation != self.generation
                    || !state.valid
                    || !state.active.as_ref().is_some_and(|active| {
                        active.managed && active.cancel.is_some() && active.ticket.id == request_id
                    })
                {
                    return false;
                }
            }
            state.paused = true;
            state.suspended = false;
            let cancel = state.active.as_mut().and_then(|active| {
                active.reason = InterruptReason::Stop;
                active.cancel.clone()
            });
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.cancel.is_none())
            {
                state.active = None;
            }
            cancel
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.reclaim().await;
        true
    }

    /// 会话关闭或持久化不确定时冻结当前 owner，旧 generation 永不重新准入。
    ///
    /// 关闭路径不能再查询 MQ；MQ 行留给重载后的 `hydrate`，这里只失效运行期状态。
    pub fn invalidate(&self) {
        let mut state = self.state.lock();
        if !state.valid {
            return;
        }
        state.valid = false;
        state.paused = true;
        state.suspended = false;
        state.active = None;
        state.ready.clear();
        for record in &mut state.records {
            if record.state == UserInputState::Dispatching {
                record.state = UserInputState::Queued;
                record.handed_off = false;
            }
        }
        state.revision += 1;
    }

    /// idle 边界与 enqueue 共用同一把锁，覆盖输入在挂起前后到达的两种顺序。
    pub(crate) async fn enter_idle(&self) {
        let ready = {
            let mut state = self.state.lock();
            state.suspended = true;
            match self.wake_suspended_locked(&mut state) {
                true => ready_ids(&state),
                false => Vec::new(),
            }
        };
        if ready.is_empty() {
            return;
        }
        self.send_ready_ids(ready, true).await;
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }

    pub(crate) fn leave_idle(&self) {
        self.state.lock().suspended = false;
    }

    pub(super) fn wake_suspended_locked(&self, state: &mut super::MailboxState) -> bool {
        // 迟到的 idle 不能恢复 Stop、立即发送收尾或已经取消的旧 attempt。
        if !state.valid
            || state.paused
            || !state.suspended
            || !state.active.as_ref().is_some_and(|active| {
                active.reason == InterruptReason::None
                    && active
                        .cancel
                        .as_ref()
                        .is_some_and(|cancel| !cancel.is_cancelled())
            })
        {
            return false;
        }
        // ready 可能在 enqueue 与 idle 之间已经完成 promotion；不能因未再次
        // promote 而跳过交接，否则输入会停在 ready 且当前执行收不到。
        if state.ready.is_empty() && !promote_next(state) {
            return false;
        }
        // 交接即占用本次 idle，后续 enqueue 不能在 Receive 开始前追加第二条。
        state.suspended = false;
        true
    }

    /// Receive 已从 MQ 独占取走这些 ID；任何 Stop 都不得再将它们恢复 queued。
    pub(crate) async fn mark_claimed(&self, ids: &[MessageId]) {
        let changed = {
            let mut state = self.state.lock();
            let mut changed = false;
            for record in &mut state.records {
                if record.state == UserInputState::Dispatching && matches_id(record, ids) {
                    record.state = UserInputState::Claimed;
                    changed = true;
                }
            }
            if changed {
                state.revision += 1;
            }
            changed
        };
        if !changed {
            return;
        }
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }
}
