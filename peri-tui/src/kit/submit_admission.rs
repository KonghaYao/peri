//! 提交车道的准入与 cohort 路由。
//!
//! 车道键是显式的会话操作身份：预约在创建时取得身份，建立/启动恢复沿用队列
//! 已登记的身份，切换与落定沿途保留同一个 [`SessionOperationId`]。因此
//! `NoSession`→`Transitioning`→`Stable` 全程共享同一条有序车道，不再根据
//! 「是否存在未认领预约」预测 `generation + 1`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::acp_client::{AcpTuiClient, AdmissionRoute, SessionOperationId};

use super::submit_consumer::SubmitLanes;

/// 会话身份 = `(session_id, lifecycle generation)`。
///
/// generation 在每次会话切换/重建时递增：同一个 session id 被重新加载也是新
/// 身份，早先排队的提交不得沿用它。
pub(super) type SessionIdentity = (String, u64);

/// 提交车道键。
///
/// 尚未落定身份的提交按操作 cohort 串行；落定后仍留在 cohort 车道，直到该
/// cohort 的队列排空，保证后来者不会越过仍在排队的前序消息。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum LaneKey {
    Cohort(CohortKey),
    Session(SessionIdentity),
}

/// 一次明确的会话操作：预约、建立或切换。
///
/// 操作身份由生命周期分配，贯穿该操作的整个生命周期。连续预约各有独立身份，
/// 队列以最新未认领预约为目标，先开始的旧预约不会把输入改道。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct CohortKey {
    operation: SessionOperationId,
}

/// cohort 的身份落定状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CohortState {
    /// 尚未落定；同 cohort 的首条提交负责解析。
    Pending,
    /// 已落定；后续成员复用它。
    Adopted(SessionIdentity),
    /// 身份已失效：剩余成员全部取消，不得把空槽当作「可重新初始化」。
    Cancelled,
}

/// cohort 的会话解析语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CohortMode {
    /// 入队时没有会话：允许在 cohort 仍有效时建立会话。
    Establish,
    /// 入队时切换已预约/进行中：只等待它落定，绝不代用户新建。
    Await,
}

/// 一个等待落定身份的 cohort。
pub(super) struct CohortEntry {
    state: Arc<Mutex<CohortState>>,
}

impl CohortEntry {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CohortState::Pending)),
        }
    }

    fn is_cancelled(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), CohortState::Cancelled)
    }

    fn ticket(
        &self,
        mode: CohortMode,
        operation: SessionOperationId,
        switch_seq: u64,
    ) -> CohortTicket {
        CohortTicket {
            state: Arc::clone(&self.state),
            mode,
            operation,
            switch_seq,
        }
    }

    /// 该 cohort 是否接受这条稳定身份（落定前/落定后都并入同一条车道）。
    fn accepts(&self, identity: &SessionIdentity) -> bool {
        match &*self.state.lock().unwrap() {
            CohortState::Adopted(adopted) => adopted == identity,
            // 键就是操作身份：同一操作的落定结果必然属于这个 cohort。
            CohortState::Pending => true,
            CohortState::Cancelled => false,
        }
    }
}

/// 一条提交对 cohort 的引用。
#[derive(Clone)]
pub(super) struct CohortTicket {
    pub(super) state: Arc<Mutex<CohortState>>,
    pub(super) mode: CohortMode,
    pub(super) operation: SessionOperationId,
    pub(super) switch_seq: u64,
}

impl CohortTicket {
    pub(super) fn is_cancelled(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), CohortState::Cancelled)
    }

    pub(super) fn adopted(&self) -> Option<SessionIdentity> {
        match &*self.state.lock().unwrap() {
            CohortState::Adopted(identity) => Some(identity.clone()),
            _ => None,
        }
    }

    /// 永久取消本 cohort：身份一旦失效，后续成员不得重新初始化。
    pub(super) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        if !matches!(&*state, CohortState::Cancelled) {
            *state = CohortState::Cancelled;
        }
    }
}

/// 一条用户提交的准入信息。
///
/// `binding` 是入队时的稳定会话身份；`cohort` 是尚未落定身份的提交所属的
/// 队列 cohort。新式服务端不占车道，无稳定会话的远程命令改用
/// `unbound_switch_seq` 在决策守卫下建立/等待会话。控制/视图请求不构造该结构。
#[derive(Clone)]
pub(super) struct SubmitAdmission {
    pub(super) binding: Option<SessionIdentity>,
    pub(super) cohort: Option<CohortTicket>,
    pub(super) unbound_switch_seq: Option<u64>,
}

/// 提交队列的 cohort 登记表。
pub(super) type Cohorts = HashMap<CohortKey, CohortEntry>;

/// 为一条排队提交登记车道与准入信息。
///
/// 目标操作按「用户最新决定」确定：未认领预约优先，其次是正在进行的切换，
/// 最后是无会话时的建立身份。稳定会话只在没有待定操作时直接绑定；仍在执行的
/// 同操作 cohort 优先，后来者不会越过前序消息。
pub(super) fn register_admission(
    acp_client: &AcpTuiClient,
    cohorts: &mut Cohorts,
    lanes: &SubmitLanes<LaneKey>,
) -> (LaneKey, SubmitAdmission) {
    let snapshot = acp_client.session_admission_snapshot();

    // 尚未被 load 认领的预约一定会推进会话，是用户最新的决定：输入必须等它
    // 落定，不能绑定当前 Stable。多个预约同时排队时取队尾，让所有输入等最终
    // 目标，而不是各自预测一次 generation。
    if let Some(operation) = snapshot.unclaimed_load {
        return cohort_admission(cohorts, operation, CohortMode::Await, snapshot.switch_seq);
    }

    match &snapshot.route {
        AdmissionRoute::Stable {
            session_id,
            generation,
            operation,
        } => {
            let identity: SessionIdentity = (session_id.clone(), *generation);
            // 同一次操作的 cohort 仍在执行：落定前后的提交必须共用车道，否则
            // 后来者会越过仍排队的前序消息（#210）。按操作身份直接取值。
            if let Some(operation) = operation {
                let key = CohortKey {
                    operation: *operation,
                };
                if let Some(entry) = cohorts.get(&key)
                    && entry.accepts(&identity)
                    && lanes.is_active(&LaneKey::Cohort(key))
                {
                    return (
                        LaneKey::Cohort(key),
                        SubmitAdmission {
                            binding: Some(identity),
                            cohort: Some(entry.ticket(
                                CohortMode::Await,
                                *operation,
                                snapshot.switch_seq,
                            )),
                            unbound_switch_seq: None,
                        },
                    );
                }
            }
            (
                LaneKey::Session(identity.clone()),
                SubmitAdmission {
                    binding: Some(identity),
                    cohort: None,
                    unbound_switch_seq: None,
                },
            )
        }
        AdmissionRoute::Transitioning { operation, .. } => {
            cohort_admission(cohorts, *operation, CohortMode::Await, snapshot.switch_seq)
        }
        AdmissionRoute::NoSession { operation, .. } => {
            // 有在途预约时只等待它落定，绝不代用户新建；无预约则允许本 cohort
            // 建立会话，失败重试沿用同一操作身份。
            let mode = if snapshot.pending_load {
                CohortMode::Await
            } else {
                CohortMode::Establish
            };
            let mut operation = *operation;
            // 同一操作已永久取消：旧票保持取消，新输入换新身份重试，不复用空槽。
            if mode == CohortMode::Establish
                && cohorts
                    .get(&CohortKey { operation })
                    .is_some_and(CohortEntry::is_cancelled)
            {
                operation = acp_client.renew_session_operation();
            }
            cohort_admission(cohorts, operation, mode, snapshot.switch_seq)
        }
    }
}

/// 把提交并入 `operation` 对应的 cohort；同键提交始终共享一条车道。
fn cohort_admission(
    cohorts: &mut Cohorts,
    operation: SessionOperationId,
    mode: CohortMode,
    switch_seq: u64,
) -> (LaneKey, SubmitAdmission) {
    let key = CohortKey { operation };
    let entry = cohorts.entry(key).or_insert_with(CohortEntry::new);
    (
        LaneKey::Cohort(key),
        SubmitAdmission {
            binding: None,
            cohort: Some(entry.ticket(mode, operation, switch_seq)),
            unbound_switch_seq: None,
        },
    )
}
