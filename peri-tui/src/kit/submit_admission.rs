//! 提交车道的准入与 cohort 路由。
//!
//! 车道键在入队时确定：同一次建立/切换从 `NoSession` 经 `Transitioning` 到
//! `Stable` 全程共享一个 [`CohortKey`]，不靠事后匹配合并。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::acp_client::{AcpTuiClient, AdmissionRoute};

use super::submit_consumer::SubmitLanes;

/// 会话身份 = `(session_id, lifecycle generation)`。
///
/// generation 在每次会话切换/重建时递增：同一个 session id 被重新加载也是新
/// 身份，早先排队的提交不得沿用它。
pub(super) type SessionIdentity = (String, u64);

/// 提交车道键。
///
/// 尚未落定身份的提交按 cohort 串行；落定后仍留在 cohort 车道，直到该 cohort
/// 的队列排空，保证后来者不会越过仍在排队的前序消息。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum LaneKey {
    Cohort(CohortKey),
    Session(SessionIdentity),
}

/// cohort 的队列键：同一次初始化/切换全程共享。
///
/// `settled_generation` 是这次操作**最终落定**的 generation，而不是当前路由
/// 观察到的代际：`NoSession(g)` 会由随后的建立推进到 `g + 1`，因此与
/// `Transitioning(g + 1)`、`Stable(_, g + 1)` 使用同一个键，队列不会在阶段
/// 交界处裂成两个可并行 cohort。`switch_seq` 区分同一代际下的不同用户决定
/// （例如建立途中又预约了历史加载），避免复用已被取代的采纳槽。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct CohortKey {
    settled_generation: u64,
    switch_seq: u64,
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
    /// 落定后应出现的 Stable generation，用于把落定前入队的提交并回同一车道。
    expected_generation: u64,
}

impl CohortEntry {
    fn new(expected_generation: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(CohortState::Pending)),
            expected_generation,
        }
    }

    fn ticket(&self, mode: CohortMode, switch_seq: u64) -> CohortTicket {
        CohortTicket {
            state: Arc::clone(&self.state),
            mode,
            switch_seq,
        }
    }

    /// 该 cohort 是否接受这条稳定身份（落定前/落定后都要并入同一条车道）。
    fn accepts(&self, identity: &SessionIdentity) -> bool {
        match &*self.state.lock().unwrap() {
            CohortState::Adopted(adopted) => adopted == identity,
            CohortState::Pending => self.expected_generation == identity.1,
            CohortState::Cancelled => false,
        }
    }
}

/// 一条提交对 cohort 的引用。
#[derive(Clone)]
pub(super) struct CohortTicket {
    pub(super) state: Arc<Mutex<CohortState>>,
    pub(super) mode: CohortMode,
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
/// 车道在路由阶段按「这次操作最终落定的 generation」确定：`NoSession(g)`、
/// `Transitioning(g + 1)` 与 `Stable(_, g + 1)` 因此共用同一条有序队列；
/// 已完成落定的 Stable 输入优先并入仍在执行的同代 cohort，否则按会话身份
/// 单独成道。不存在「先分道、再任取匹配项合并」的路径。
pub(super) fn register_admission(
    acp_client: &AcpTuiClient,
    cohorts: &mut Cohorts,
    lanes: &SubmitLanes<LaneKey>,
) -> (LaneKey, SubmitAdmission) {
    let snapshot = acp_client.session_admission_snapshot();

    // 尚未被 load 认领的预约一定会推进 generation，是用户最新的会话决定：
    // 输入必须等它落定，不能绑定当前 Stable。已认领的预约不再预测下一代
    // ——那正是「load 已 commit、预约未释放」窗口把后续输入拆道的原因。
    if snapshot.unclaimed_load {
        let expected = snapshot.route.generation().saturating_add(1);
        return cohort_admission(
            cohorts,
            CohortKey {
                settled_generation: expected,
                switch_seq: snapshot.switch_seq,
            },
            CohortMode::Await,
            expected,
        );
    }

    match &snapshot.route {
        AdmissionRoute::Stable {
            session_id,
            generation,
        } => {
            let identity: SessionIdentity = (session_id.clone(), *generation);
            let key = CohortKey {
                settled_generation: *generation,
                switch_seq: snapshot.switch_seq,
            };
            // 同代 cohort 仍在执行：落定前后的提交必须共用车道，否则后来者
            // 会越过仍排队的前序消息（#210）。按 generation 直接取值，不做
            // 任意匹配。
            if let Some(entry) = cohorts.get(&key)
                && entry.accepts(&identity)
                && lanes.is_active(&LaneKey::Cohort(key))
            {
                return (
                    LaneKey::Cohort(key),
                    SubmitAdmission {
                        binding: Some(identity),
                        cohort: Some(entry.ticket(CohortMode::Await, key.switch_seq)),
                        unbound_switch_seq: None,
                    },
                );
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
        AdmissionRoute::Transitioning { generation } => cohort_admission(
            cohorts,
            CohortKey {
                settled_generation: *generation,
                switch_seq: snapshot.switch_seq,
            },
            CohortMode::Await,
            *generation,
        ),
        AdmissionRoute::NoSession { generation } => {
            let expected = generation.saturating_add(1);
            cohort_admission(
                cohorts,
                CohortKey {
                    settled_generation: expected,
                    switch_seq: snapshot.switch_seq,
                },
                if snapshot.pending_load {
                    CohortMode::Await
                } else {
                    CohortMode::Establish
                },
                expected,
            )
        }
    }
}

/// 把提交并入 `key` 对应的 cohort；同键提交始终共享一条车道。
fn cohort_admission(
    cohorts: &mut Cohorts,
    key: CohortKey,
    mode: CohortMode,
    expected_generation: u64,
) -> (LaneKey, SubmitAdmission) {
    let entry = cohorts
        .entry(key)
        .or_insert_with(|| CohortEntry::new(expected_generation));
    (
        LaneKey::Cohort(key),
        SubmitAdmission {
            binding: None,
            cohort: Some(entry.ticket(mode, key.switch_seq)),
            unbound_switch_seq: None,
        },
    )
}
