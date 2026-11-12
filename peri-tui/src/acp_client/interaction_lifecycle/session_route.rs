//! Session route transitions and operation-identity bookkeeping.

use super::leases::TransitionLease;
use super::reverse::drain_pending;
use super::{
    AdmissionRoute, ClaimCause, ClaimedInteraction, InteractionLifecycle,
    InteractionLifecycleState, SessionOperationId, SessionRoute, TransitionKind,
    TransitionOperation, TransitionStart,
};

#[cfg(test)]
use super::PromptMarker;

impl SessionOperationId {
    fn next(state: &mut InteractionLifecycleState) -> Self {
        let id = state.next_operation_id;
        state.next_operation_id = state
            .next_operation_id
            .checked_add(1)
            .expect("session operation id overflow");
        Self(id)
    }
}

impl SessionRoute {
    fn generation(&self) -> u64 {
        match self {
            Self::Bootstrapping { generation }
            | Self::Stable { generation, .. }
            | Self::Transitioning { generation, .. }
            | Self::NoSession { generation } => *generation,
        }
    }
}

impl InteractionLifecycle {
    pub fn has_session(&self) -> bool {
        matches!(
            self.state.lock().unwrap().route,
            SessionRoute::Stable { .. }
        )
    }

    pub fn current_session_id(&self) -> Option<String> {
        match &self.state.lock().unwrap().route {
            SessionRoute::Stable { session_id, .. } => Some(session_id.clone()),
            SessionRoute::Transitioning { target, .. } => target.clone(),
            SessionRoute::Bootstrapping { .. } | SessionRoute::NoSession { .. } => None,
        }
    }

    pub fn stable_identity(&self) -> Option<(String, u64)> {
        match &self.state.lock().unwrap().route {
            SessionRoute::Stable {
                session_id,
                generation,
                ..
            } => Some((session_id.clone(), *generation)),
            _ => None,
        }
    }

    /// Stable 身份及其产生它的操作；准入在同一临界区内判定身份归属。
    pub(crate) fn stable_identity_with_operation(
        &self,
    ) -> Option<(String, u64, Option<SessionOperationId>)> {
        match &self.state.lock().unwrap().route {
            SessionRoute::Stable {
                session_id,
                generation,
                operation,
                ..
            } => Some((session_id.clone(), *generation, *operation)),
            _ => None,
        }
    }

    /// 排队提交看到的路由快照（含操作身份），一次加锁读出。
    ///
    /// `NoSession` 没有在途操作时按需分配一个建立身份并记住它：后续输入与
    /// 失败后的重试继续使用同一身份，车道不会因为「还没有操作」而分裂。
    pub(crate) fn admission_route(&self) -> AdmissionRoute {
        let mut state = self.state.lock().unwrap();
        match state.route.clone() {
            SessionRoute::Stable {
                session_id,
                generation,
                operation,
                ..
            } => AdmissionRoute::Stable {
                session_id,
                generation,
                operation,
            },
            SessionRoute::Transitioning {
                generation,
                operation,
                ..
            } => AdmissionRoute::Transitioning {
                generation,
                operation,
            },
            SessionRoute::Bootstrapping { generation } | SessionRoute::NoSession { generation } => {
                let operation = match state.current_operation {
                    Some(operation) => operation,
                    None => {
                        let operation = SessionOperationId::next(&mut state);
                        state.current_operation = Some(operation);
                        operation
                    }
                };
                AdmissionRoute::NoSession {
                    generation,
                    operation,
                }
            }
        }
    }

    /// 新的一次用户决定：分配新身份并记为当前待定身份。
    pub(crate) fn allocate_operation(&self) -> SessionOperationId {
        let mut state = self.state.lock().unwrap();
        let operation = SessionOperationId::next(&mut state);
        state.current_operation = Some(operation);
        operation
    }

    /// 新的一次用户决定：分配新身份。
    pub fn begin_transition(
        &self,
        kind: TransitionKind,
        target: Option<String>,
    ) -> Result<TransitionStart, &'static str> {
        self.begin_transition_with_operation(kind, target, TransitionOperation::New)
    }

    /// 用明确的身份来源开始切换；`Owned` 让同一预约/cohort 贯穿全部阶段。
    pub(crate) fn begin_transition_with_operation(
        &self,
        kind: TransitionKind,
        target: Option<String>,
        operation: TransitionOperation,
    ) -> Result<TransitionStart, &'static str> {
        let mut state = self.state.lock().unwrap();
        let generation = state
            .route
            .generation()
            .checked_add(1)
            .ok_or("session generation exhausted")?;
        let operation = match operation {
            TransitionOperation::Owned(operation) => {
                // 旧决定可能仍在执行（例如先后预约 B、C 时 B 先开始）；
                // 不覆盖更新的待定身份，队列仍以最新决定为准。
                state.current_operation.get_or_insert(operation);
                operation
            }
            TransitionOperation::AdoptPending => match state.current_operation {
                Some(operation) => operation,
                None => {
                    let operation = SessionOperationId::next(&mut state);
                    state.current_operation = Some(operation);
                    operation
                }
            },
            TransitionOperation::New => {
                let operation = SessionOperationId::next(&mut state);
                state.current_operation = Some(operation);
                operation
            }
        };
        let from = match &state.route {
            SessionRoute::Stable { session_id, .. } => Some(session_id.clone()),
            SessionRoute::Transitioning { target, .. } => target.clone(),
            _ => None,
        };
        state.route = SessionRoute::Transitioning {
            kind,
            from: from.clone(),
            target,
            generation,
            operation,
        };
        state.active_prompt = None;
        state.new_buffer.clear();
        state.new_flush_pending = false;
        state.warned_buffer_full = false;
        let claims = drain_pending(&mut state, ClaimCause::LifecycleDrain, |_| true);
        Ok(TransitionStart {
            generation,
            from,
            claims,
        })
    }

    pub fn arm_transition(&self, generation: u64) -> TransitionLease {
        TransitionLease {
            lifecycle: self.clone(),
            generation,
            armed: true,
        }
    }

    /// 切到 Stable 路由，但保留 New 期间缓存的普通通知。
    ///
    /// 调用方随后仍要完成初始化（可能等待 `session/input/snapshot`）。这段窗口
    /// 内到达的通知必须继续入队，否则会先于缓存帧投递并破坏 wire FIFO；缓存由
    /// [`Self::take_buffered_batch`] 在初始化结束后原子排空。
    pub fn commit_stable(&self, generation: u64, session_id: String) {
        let mut state = self.state.lock().unwrap();
        if state.route.generation() != generation {
            return;
        }
        // 落定身份保留产生它的操作：排队输入据此确认「这就是我在等的操作」。
        let operation = match &state.route {
            SessionRoute::Transitioning { operation, .. } => Some(*operation),
            SessionRoute::Stable { operation, .. } => *operation,
            _ => None,
        };
        state.route = SessionRoute::Stable {
            session_id,
            generation,
            operation,
            accepting_reverse: false,
            prompt_epoch: 0,
        };
        state.active_prompt = None;
        state.warned_buffer_full = false;
        state.new_flush_pending = true;
    }

    pub fn fail_transition(&self, generation: u64) -> Vec<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        if state.route.generation() != generation {
            return Vec::new();
        }
        state.route = SessionRoute::NoSession { generation };
        state.active_prompt = None;
        state.new_buffer.clear();
        state.new_flush_pending = false;
        state.warned_buffer_full = false;
        drain_pending(&mut state, ClaimCause::LifecycleDrain, |_| true)
    }

    /// initial 失败但旧窗口仍有效：回收本次切换的排队认领，并把路由交还旧会话。
    ///
    /// 服务端失败路径会先把候选期更新补回旧流；路由若停在 `NoSession`，pump 会
    /// 把这些真实通知直接丢弃，恢复的窗口只剩下快照。旧会话的实时/补发通知必须
    /// 继续进入 notifier，由已恢复的窗口按 seq 接纳。
    pub(crate) fn fail_transition_restoring_route(
        &self,
        generation: u64,
        session_id: &str,
    ) -> Vec<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        if state.route.generation() != generation {
            return Vec::new();
        }
        let operation = match &state.route {
            SessionRoute::Transitioning { operation, .. } => Some(*operation),
            SessionRoute::Stable { operation, .. } => *operation,
            _ => None,
        };
        state.route = SessionRoute::Stable {
            session_id: session_id.to_owned(),
            generation,
            operation,
            accepting_reverse: false,
            prompt_epoch: 0,
        };
        state.active_prompt = None;
        state.new_buffer.clear();
        state.new_flush_pending = false;
        state.warned_buffer_full = false;
        drain_pending(&mut state, ClaimCause::LifecycleDrain, |_| true)
    }

    pub fn mark_deleted(&self, session_id: &str) {
        let mut state = self.state.lock().unwrap();
        state.deleted_session_ids.insert(session_id.to_string());
    }

    pub fn is_current_session(&self, session_id: &str) -> bool {
        if session_id.is_empty() {
            return true;
        }
        let state = self.state.lock().unwrap();
        if state.deleted_session_ids.contains(session_id) {
            return false;
        }
        match &state.route {
            SessionRoute::Stable {
                session_id: current,
                ..
            } => current == session_id,
            SessionRoute::Transitioning {
                kind: TransitionKind::Load,
                target: Some(target),
                ..
            } => target == session_id,
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn force_stable(&self, session_id: &str, accepting_reverse: bool) {
        let mut state = self.state.lock().unwrap();
        state.route = SessionRoute::Stable {
            session_id: session_id.to_string(),
            generation: 1,
            operation: None,
            accepting_reverse,
            prompt_epoch: u64::from(accepting_reverse),
        };
        state.active_prompt = accepting_reverse.then(|| PromptMarker {
            client_instance_id: state.client_instance_id,
            session_id: session_id.to_string(),
            generation: 1,
            prompt_epoch: 1,
            request_id: Some("test-prompt".into()),
        });
    }
}
