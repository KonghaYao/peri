//! child resume 认领 handle：认领期间的状态写入与恢复由资源内部持有。
//!
//! 调用方只报告领域结果（开始运行 / 移交后台 / 准备失败 / 终止），不拼补偿写入、
//! 不接触事务或重试令牌。handle 保存认领前的记录，终态方法把它写回去——
//! 「恢复到认领前」这件事只有一个实现。

use std::sync::Mutex;

use async_trait::async_trait;
use peri_acp_types::session_resources::{
    ChildResumeClaim, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::{AgentStatus, ThreadId};

use super::gate::MutationGate;
use crate::sessions::data::ChildResumeRecord;

/// 一次认领所处的阶段。
enum ClaimPhase {
    /// 认领已写入 active，准备阶段尚未结束。
    Preparing,
    /// 调用方已声明认领成功并开始运行。
    Running,
    /// 已移交后台执行：终态状态由后台持有，本 handle 不再改写它。
    HandedOff,
    /// 已按领域结果收尾（失败或终止），恢复了认领前的记录。
    Settled,
}

pub(super) struct ChildResumeClaimHandle {
    gate: MutationGate,
    child: ThreadId,
    previous: ChildResumeRecord,
    phase: Mutex<ClaimPhase>,
}

impl ChildResumeClaimHandle {
    pub(super) fn new(gate: MutationGate, child: ThreadId, previous: ChildResumeRecord) -> Self {
        Self {
            gate,
            child,
            previous,
            phase: Mutex::new(ClaimPhase::Preparing),
        }
    }

    /// 认领期间的写入走同一套准入检查（能力/未决持久化/root owner）。
    async fn write(&self, record: &ChildResumeRecord) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(&self.child, || {
                self.gate
                    .data()
                    .store_child_resume_record(&self.child, record)
            })
            .await
    }

    fn phase(&self) -> Result<std::sync::MutexGuard<'_, ClaimPhase>, SessionResourceError> {
        self.phase.lock().map_err(|_| {
            SessionResourceError::new(SessionResourceErrorKind::Internal {
                detail: "child resume claim state is poisoned".to_owned(),
            })
        })
    }

    fn already_settled() -> SessionResourceError {
        SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
            detail: "child resume claim is already settled".to_owned(),
        })
    }

    fn handed_off() -> SessionResourceError {
        SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
            detail: "child resume claim was handed off to background execution".to_owned(),
        })
    }
}

#[async_trait]
impl ChildResumeClaim for ChildResumeClaimHandle {
    async fn mark_running(&self) -> SessionResourceResult<()> {
        {
            let mut phase = self.phase()?;
            match *phase {
                ClaimPhase::Preparing => *phase = ClaimPhase::Running,
                ClaimPhase::Running => {}
                ClaimPhase::HandedOff | ClaimPhase::Settled => return Err(Self::already_settled()),
            }
        }
        // 运行态在持久化里就是「active 且已认领」：再确认一次，让记录与调用方声明一致。
        self.write(&ChildResumeRecord {
            status: AgentStatus::Active,
            claimed: true,
        })
        .await
    }

    async fn hand_off_to_background(&self) -> SessionResourceResult<()> {
        {
            let mut phase = self.phase()?;
            match *phase {
                ClaimPhase::Preparing | ClaimPhase::Running => *phase = ClaimPhase::HandedOff,
                ClaimPhase::HandedOff => {}
                ClaimPhase::Settled => return Err(Self::already_settled()),
            }
        }
        self.write(&ChildResumeRecord {
            status: AgentStatus::Active,
            claimed: true,
        })
        .await
    }

    async fn mark_failed(&self) -> SessionResourceResult<()> {
        self.settle().await
    }

    async fn mark_terminated(&self) -> SessionResourceResult<()> {
        self.settle().await
    }
}

impl ChildResumeClaimHandle {
    /// 收尾：恢复到认领前的记录，不留 active 残留。
    ///
    /// 已移交后台时拒绝改写——后台执行才是终态的持有者，前台的终止声明不能覆盖它。
    async fn settle(&self) -> SessionResourceResult<()> {
        {
            let mut phase = self.phase()?;
            match *phase {
                ClaimPhase::Preparing | ClaimPhase::Running => *phase = ClaimPhase::Settled,
                ClaimPhase::Settled => {}
                ClaimPhase::HandedOff => return Err(Self::handed_off()),
            }
        }
        self.write(&self.previous.clone()).await
    }
}
