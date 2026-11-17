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
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};

use super::gate::MutationGate;
use crate::sessions::data::ChildResumeRecord;

/// 一次认领所处的阶段。
#[derive(Clone, Copy)]
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
    previous_work_state: WorkState,
    /// 本次认领的进程内世代；新认领会覆盖它，旧 handle 随之失去补偿资格。
    claim_nonce: u64,
    phase: Mutex<ClaimPhase>,
}

impl ChildResumeClaimHandle {
    pub(super) fn new(
        gate: MutationGate,
        child: ThreadId,
        previous: ChildResumeRecord,
        previous_work_state: WorkState,
        claim_nonce: u64,
    ) -> Self {
        Self {
            gate,
            child,
            previous,
            previous_work_state,
            claim_nonce,
            phase: Mutex::new(ClaimPhase::Preparing),
        }
    }

    /// 认领期间的写入走同一套准入检查（权限/未决持久化/同树 mutation 屏障）。
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
        self.settle(true, None).await.map(|_| ())
    }

    async fn mark_terminated(&self) -> SessionResourceResult<()> {
        self.settle(false, None).await.map(|_| ())
    }

    async fn finish_with_status(&self, status: AgentStatus) -> SessionResourceResult<bool> {
        self.settle(false, Some(status)).await
    }
}

/// 写侧门禁内一次收尾的结果。
enum SettleOutcome {
    /// 世代或认领记录已不属于本次认领：没有写入。
    NotOwned,
    /// 恢复与终态写入均已完成。
    Settled,
}

impl ChildResumeClaimHandle {
    /// 收尾：恢复到认领前的状态与工作意图，不留 active 残留；请求终态时在同一
    /// 归属校验内追加唯一一次终态写入。
    ///
    /// 后台尚未启动时可补偿移交准备失败；终止声明不能覆盖后台执行持有的终态。
    /// 归属校验、两列恢复与终态写入在同一写侧门禁与同一次提交内完成，新任务无法
    /// 插入到「恢复」与「终态写入」之间。校验用进程内认领世代，而不是当前记录值——新 run
    /// 的正常状态与旧认领的中间态完全相同，按值判断会把新 run 误认为自己的记录。
    async fn settle(
        &self,
        preparation_failed: bool,
        terminal: Option<AgentStatus>,
    ) -> SessionResourceResult<bool> {
        // 写入成功后才置 Settled：失败时保留原阶段，允许后续调用重试。
        let previous_phase = {
            let phase = self.phase()?;
            match *phase {
                ClaimPhase::Preparing | ClaimPhase::Running => *phase,
                // 重复结清是 no-op：迟到的旧结清不能再次写回认领前的值，
                // 否则会覆盖新任务已经提交的 Active + Working。
                ClaimPhase::Settled => return Ok(false),
                ClaimPhase::HandedOff if preparation_failed && terminal.is_none() => *phase,
                ClaimPhase::HandedOff => return Err(Self::handed_off()),
            }
        };
        let outcome = self
            .gate
            .with_exclusive(&self.child, || async {
                if !self.gate.claim_is_current(&self.child, self.claim_nonce) {
                    tracing::warn!(
                        thread_id = %self.child,
                        claim_nonce = self.claim_nonce,
                        "resume claim rollback skipped: a newer claim owns this child"
                    );
                    return Ok(SettleOutcome::NotOwned);
                }
                let current = self
                    .gate
                    .data()
                    .load_child_resume_record(&self.child)
                    .await?;
                let current_work_state = self.gate.data().load_meta(&self.child).await?.work_state;
                let claimed = ChildResumeRecord {
                    status: AgentStatus::Active,
                    claimed: true,
                };
                if current != claimed {
                    // 状态列已被第三方改写：恢复与终态都不再属于本次认领。
                    tracing::warn!(
                        thread_id = %self.child,
                        expected_status = ?claimed.status,
                        current_status = ?current.status,
                        current_claimed = current.claimed,
                        "resume claim rollback skipped: record no longer belongs to this claim"
                    );
                    return Ok(SettleOutcome::NotOwned);
                }
                // 工作意图可能已被本次运行写成终态（run_sync 在提交终态前先写
                // work_state）：此时只跳过这一列的回滚，终态仍由本次认领提交。
                if current_work_state == WorkState::Working
                    || current_work_state == self.previous_work_state
                {
                    let work_state = (current_work_state != self.previous_work_state)
                        .then_some(self.previous_work_state);
                    // 恢复与终态在同一次提交里完成：只写好其中一项就失败会留下不再
                    // 属于本次认领的记录，后续补偿会被当成他人写入而失去重试责任。
                    self.gate
                        .data()
                        .settle_child_resume(&self.child, &self.previous, work_state, terminal)
                        .await?;
                } else if let Some(status) = terminal {
                    // 终态写入失败必须从闭包返回 Err：Unknown 由门禁提升不确定屏障，
                    // 且本次认领不得被标记为已结算或回收。
                    self.gate
                        .data()
                        .store_child_resume_record(
                            &self.child,
                            &ChildResumeRecord {
                                status,
                                claimed: false,
                            },
                        )
                        .await?;
                }
                Ok(SettleOutcome::Settled)
            })
            .await;
        {
            let mut phase = self.phase()?;
            match &outcome {
                Ok(_) => *phase = ClaimPhase::Settled,
                Err(_) => *phase = previous_phase,
            }
        }
        if outcome.is_ok() {
            // 成功/放弃补偿后不再需要守卫；按世代回收，不会移除新认领的条目。
            self.gate.release_claim(&self.child, self.claim_nonce);
        }
        match outcome {
            Ok(SettleOutcome::NotOwned) => Ok(false),
            Ok(SettleOutcome::Settled) => Ok(true),
            Err(error) => Err(error),
        }
    }
}

impl Drop for ChildResumeClaimHandle {
    fn drop(&mut self) {
        // handle 离开作用域后不可能再补偿，条目不再需要守卫；世代匹配保证不碰新认领。
        self.gate.release_claim(&self.child, self.claim_nonce);
    }
}
