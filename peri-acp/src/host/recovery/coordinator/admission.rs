//! 提醒准入：生命周期复核、进程内去重、外围任务查询与入队放行。
//!
//! 根与冷 child 共用同一条复核/注入序列；拆分只按职责分组，不改变判断顺序。

use std::sync::Arc;

use peri_acp_types::ports::{McpPoolPort, PeripheralTasks};
use peri_acp_types::recovery::{
    RecoveryError, RecoveryErrorKind, RecoveryFailure, RecoverySkipReason,
};
use peri_acp_types::session::{MessageQueue, MqEnqueueDisposition, MqEnqueueOutcome};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use peri_acp_types::thread::ThreadId;

use super::super::reminder;
#[cfg(test)]
use super::admission_gate;
use super::{
    AdmissionPhase, AdmittedRecovery, HostSessionRecovery, LifecycleSnapshot, ThreadOutcome,
};

impl HostSessionRecovery {
    /// 已装配 thread 的提醒准入：根与冷 child 共用同一条复核/注入序列。
    ///
    /// `fatal` 只对「进入的会话就是根」成立：此时失败必须成为请求错误；child 的
    /// 失败由调用方降级为 Skip。
    pub(super) async fn admit_thread(
        &self,
        root: &ThreadId,
        thread: &ThreadId,
        session_id: &str,
        direct_children: &[ThreadId],
        parent: Option<ThreadId>,
        fatal: bool,
    ) -> Result<ThreadOutcome, RecoveryError> {
        let Some(before) = self.lifecycle(session_id).await else {
            return self.thread_failure(thread, "session state is not published", fatal);
        };
        if before.turn_active {
            // 已有实例正在执行本 thread：连接现有运行，不重复注入。运行中的 turn
            // 会消费 MQ 中尚未移交的提醒，本次准入资格随之失效。恢复 child 的直接
            // 父级若此时才 live，在这里补登记（adoption）。
            self.admitted.remove(thread);
            if parent.is_some() {
                self.adopt_live(thread).await;
            }
            return Ok(ThreadOutcome::Reused);
        }
        if before.closing {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Stopped));
        }
        let Some(runtime) = self.manager.get_session(session_id) else {
            return self.thread_failure(thread, "session runtime is not registered", fatal);
        };
        let activation = Arc::clone(&runtime.activation);
        drop(runtime);
        // 显式 Stop 后不得经恢复重新放行执行。
        if activation.user_stopped() {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Stopped));
        }

        let queue = self.queue_for(thread);
        // 防重复的唯一进程内事实是 admitted；MQ 行只表示「有一条可复用的待消费提醒」。
        // 这样 `discard`、响应失败或进程内表项被清理后，下一次进入仍能采纳旧提醒，
        // 而不会把它误判成永久 AlreadyRunning。
        //
        // 上一次启动的 continuation 在 Receive 前失败时，进程内标记会说明「启动过但
        // 没跑起来」：此时清除 Launched 占位，按同一条提醒重新准入。只在这里消费
        // 标记，因此重试只由显式进入触发，不形成恢复循环。
        let launch_failed = self.take_recovery_launch_failed(session_id).await;
        let mut inject = true;
        let mut adopted_seed = None;
        if let Some(entry) = self.admitted.get(thread).map(|entry| entry.clone()) {
            let same_lifecycle =
                entry.generation == before.generation && entry.session_id == session_id;
            if !same_lifecycle {
                self.admitted.remove_if(thread, |_, current| {
                    current.generation == entry.generation && current.session_id == entry.session_id
                });
            } else if entry.phase == AdmissionPhase::Prepared {
                return Ok(ThreadOutcome::Skipped(RecoverySkipReason::AlreadyRunning));
            } else if launch_failed {
                self.admitted.remove_if(thread, |_, current| {
                    current.generation == entry.generation
                        && current.session_id == entry.session_id
                        && current.phase == AdmissionPhase::Launched
                });
            } else {
                match reminder::find_pending_recovery(&queue).await {
                    Ok(Some(_)) => {
                        return Ok(ThreadOutcome::Skipped(RecoverySkipReason::AlreadyRunning))
                    }
                    Ok(None) => {
                        // 提醒已被上一次启动的 turn 消费：表项过期，继续按当前事实重新判定。
                        self.admitted.remove_if(thread, |_, current| {
                            current.generation == entry.generation
                                && current.session_id == entry.session_id
                                && current.phase == AdmissionPhase::Launched
                        });
                    }
                    Err(error) => {
                        // 已有本进程启动证据：查询失败时保守不复注第二条。
                        tracing::warn!(
                            thread_id = %thread,
                            %error,
                            "recovery: pending reminder recheck failed"
                        );
                        return Ok(ThreadOutcome::Skipped(RecoverySkipReason::AlreadyRunning));
                    }
                }
            }
        }

        match reminder::find_pending_recovery(&queue).await {
            Ok(Some(entry)) => {
                inject = false;
                // 采纳同一条提醒：结果身份沿用该提醒，重复进入仍幂等。
                adopted_seed = Some(entry.id);
            }
            Ok(None) => {}
            Err(error) if fatal => {
                return Err(RecoveryError::from(error).with_thread(thread));
            }
            Err(error) => {
                tracing::warn!(
                    thread_id = %thread,
                    %error,
                    "recovery: pending reminder query failed"
                );
                return Ok(ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed));
            }
        }

        // 注入前复核：等待根锁或读写期间会话可能已换代或个人已停止。
        if let Some(reason) = self.lifecycle_skip(session_id, before).await {
            return Ok(ThreadOutcome::Skipped(reason));
        }

        // 设计 §6：外围任务只按本 thread 的直接 owner 查询。查询失败 / 未接线
        // 都是 Unknown，绝不写成「没有任务」；它也不阻断本 thread 的恢复。
        let peripheral_tasks = self.peripheral_tasks(session_id, thread).await;

        // 外围查询同样是异步窗口：结束之后、入队之前重新核对一次，旧快照不得
        // 覆盖快照之后发生的 Stop、关闭、换代或新的 turn。
        if let Some(reason) = self.lifecycle_skip(session_id, before).await {
            return Ok(ThreadOutcome::Skipped(reason));
        }
        if activation.user_stopped() {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Stopped));
        }

        // 提醒构造失败时不改变执行资格：先构造，再尝试放行。
        let trusted = if inject {
            match reminder::recovery_reminder(root, thread, direct_children, &peripheral_tasks) {
                Ok(trusted) => Some(trusted),
                Err(detail) => {
                    return self.thread_failure(
                        thread,
                        format!("recovery reminder was rejected: {detail}"),
                        fatal,
                    )
                }
            }
        } else {
            None
        };

        #[cfg(test)]
        admission_gate::wait(thread.as_str(), admission_gate::PausePoint::BeforeAdmit).await;

        // 放行与 Stop 在同一把锁内线性化：复核之后落下的 Stop 不能被这里清除。
        if !activation.try_allow() {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Stopped));
        }
        // 恢复启动门：从提醒采纳到 recovery 续跑取得准入期间，普通激活不得为同一条
        // 提醒并发下单；否则两条续跑会在 prompt lock 上竞争，普通轮次的早失败会
        // 污染恢复的工作意图。提交准入、调度器跳过或响应丢弃时解除。
        activation.begin_recovery_launch();
        let mut settlement_seed = adopted_seed;
        let mut enqueued = None;
        if let Some(trusted) = trusted {
            // 提醒接纳要能通过 `pending_request` 的激活门：放行执行资格在启动前最后一次复核。
            let outcome = match queue.enqueue(reminder::queued_recovery(trusted)).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    activation.mark_recovery_launch_failed();
                    activation.finish_recovery_launch();
                    return self.thread_failure(thread, error.to_string(), fatal);
                }
            };
            settlement_seed = Some(outcome.entry.id);
            // 入队也是异步窗口：结果若已换代或被 Stop，撤销本次提醒，不能把它带进
            // 新生命周期。存储不支持取消提醒时保留，下一次进入会采纳同一条。
            if let Some(reason) = self.post_admit_skip(session_id, before, &activation).await {
                withdraw_new_reminder(&queue, &outcome).await;
                activation.mark_recovery_launch_failed();
                activation.finish_recovery_launch();
                return Ok(ThreadOutcome::Skipped(reason));
            }
            enqueued = Some(outcome);
        } else {
            // 采纳旧提醒：不重复入队；任务实时状态由当前 turn 的工具查询修正。
            if let Some(reason) = self.post_admit_skip(session_id, before, &activation).await {
                activation.mark_recovery_launch_failed();
                activation.finish_recovery_launch();
                return Ok(ThreadOutcome::Skipped(reason));
            }
        }
        // 结果通知挂在会话实例上：本轮无论由启动门、激活监听还是用户 prompt 发起，
        // 收尾都能投递一次（不依赖哪一条续跑请求真正跑到）。
        if let Some(parent) = parent.clone() {
            if let Some(runtime) = self.manager.get_session(session_id) {
                if !runtime.record_recovery_parent(parent, settlement_seed) {
                    // 上一轮已登记父任务但尚未结算：不得覆盖它的投递与任务身份。
                    if let Some(outcome) = &enqueued {
                        withdraw_new_reminder(&queue, outcome).await;
                    }
                    activation.mark_recovery_launch_failed();
                    activation.finish_recovery_launch();
                    return Ok(ThreadOutcome::Skipped(RecoverySkipReason::AlreadyRunning));
                }
            }
        }
        let admission = AdmittedRecovery {
            generation: before.generation,
            session_id: session_id.to_owned(),
            phase: AdmissionPhase::Prepared,
        };
        self.admitted.insert(thread.clone(), admission.clone());
        Ok(ThreadOutcome::Injected {
            peripheral_tasks,
            admission,
        })
    }

    /// 生命周期复核：同一代次、未关闭且没有新 turn 才能继续本次准备。
    async fn lifecycle_skip(
        &self,
        session_id: &str,
        before: LifecycleSnapshot,
    ) -> Option<RecoverySkipReason> {
        let Some(current) = self.lifecycle(session_id).await else {
            return Some(RecoverySkipReason::Superseded);
        };
        if current.generation != before.generation {
            return Some(RecoverySkipReason::Superseded);
        }
        if current.closing {
            return Some(RecoverySkipReason::Stopped);
        }
        if current.turn_active {
            return Some(RecoverySkipReason::AlreadyRunning);
        }
        None
    }

    /// 放行之后、登记之前的复核：Stop 也会复位 `allowed`，两处都要看。
    async fn post_admit_skip(
        &self,
        session_id: &str,
        before: LifecycleSnapshot,
        activation: &crate::session::SessionActivation,
    ) -> Option<RecoverySkipReason> {
        if let Some(reason) = self.lifecycle_skip(session_id, before).await {
            return Some(reason);
        }
        if activation.user_stopped() || !activation.is_allowed() {
            return Some(RecoverySkipReason::Stopped);
        }
        None
    }

    /// 逐项失败分类：`fatal` 时上抛请求错误，否则只进报告。
    fn thread_failure(
        &self,
        thread: &ThreadId,
        detail: impl Into<String>,
        fatal: bool,
    ) -> Result<ThreadOutcome, RecoveryError> {
        let failure = RecoveryFailure {
            thread_id: thread.clone(),
            kind: RecoveryErrorKind::Unavailable,
            detail: detail.into(),
        };
        if fatal {
            return Err(self.fatal(thread, &failure));
        }
        Ok(ThreadOutcome::Failed(failure))
    }

    /// 本 thread 的外围存活任务查询（设计 §6，只读）。
    ///
    /// 会话环境里的池是会话装配产出的那份；宿主级装配没有池时回落到自身配置。
    /// 拿不到任何池就是 `Unknown`——这不是「没有任务」。
    async fn peripheral_tasks(&self, session_id: &str, owner: &ThreadId) -> PeripheralTasks {
        let session_pool = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(session_id)
                .and_then(|state| state.environment.as_ref())
                .and_then(|environment| environment.cfg.mcp_pool.clone())
        };
        let pool: Option<std::sync::Arc<dyn McpPoolPort>> =
            session_pool.or_else(|| self.cfg.mcp_pool.clone());
        let Some(pool) = pool else {
            return PeripheralTasks::unknown("session MCP pool is not live in this process");
        };
        pool.workspace_live_tasks(owner.as_str()).await
    }

    fn fatal(&self, root: &ThreadId, failure: &RecoveryFailure) -> RecoveryError {
        RecoveryError::new(
            failure.kind,
            Some(root.clone()),
            SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                detail: failure.detail.clone(),
            }),
        )
    }

    /// 读取并清除「上一次恢复启动在 Receive 前失败」的进程内事实。
    ///
    /// 标记由 prompt dispatch 在恢复 continuation 早失败时写入；只有显式进入读取，
    /// 且 take 语义保证同一次失败最多触发一次重新准入。
    async fn take_recovery_launch_failed(&self, session_id: &str) -> bool {
        self.manager
            .get_session(session_id)
            .is_some_and(|session| session.take_recovery_launch_failed())
    }
}

/// 撤销本次刚接纳的提醒。
///
/// MQ 目前只允许取消用户输入；存储拒绝时提醒保留，下一次进入会采纳同一条而不是
/// 重复注入（与 `discard` 语义相同）。
async fn withdraw_new_reminder(queue: &MessageQueue, outcome: &MqEnqueueOutcome) {
    if outcome.disposition != MqEnqueueDisposition::Inserted {
        return;
    }
    if let Err(error) = queue.cancel_user_input(&outcome.entry.id).await {
        tracing::debug!(
            error = %error,
            "recovery: reminder withdraw is not supported; the next entry will adopt it"
        );
    }
}
