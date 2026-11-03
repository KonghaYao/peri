use super::{
    BackgroundRegistryError, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle,
};
use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind};
use tracing::warn;

const CANCEL_GRACE_SECS: u64 = 3;

impl BackgroundTaskRegistry {
    /// 取消指定任务（按 BgCancelHandle 分发取消逻辑）
    pub fn cancel(&self, task_id: &str) -> Result<(), BackgroundRegistryError> {
        let mut tasks = self.tasks.lock();
        let Some(status) = tasks.get(task_id).map(|task| &task.status) else {
            return Err(BackgroundRegistryError::TaskNotFound(task_id.to_string()));
        };
        if matches!(status, BackgroundTaskStatus::Completing) {
            return Err(BackgroundRegistryError::TaskCompleting(task_id.to_string()));
        }
        if !matches!(status, BackgroundTaskStatus::Running) {
            return Err(BackgroundRegistryError::TaskNotFound(task_id.to_string()));
        }
        // 先校验取消句柄可用性：Kill(None) 表示 kill 通道不可用（如 workflow kill 闭包缺失、
        // shell spawn 失败），此时如实返回错误并保留条目，等待任务自然完成，
        // 而不是移除条目 + 发 cancelled 事件假装成功（issue 2026-08-05）。
        let handle_unavailable = matches!(
            tasks.get(task_id).map(|t| &t.cancel_handle),
            Some(BgCancelHandle::Kill(None))
        );
        if handle_unavailable {
            return Err(BackgroundRegistryError::KillUnavailable(
                task_id.to_string(),
            ));
        }
        if matches!(
            tasks.get(task_id).map(|t| &t.cancel_handle),
            Some(BgCancelHandle::External { .. })
        ) {
            return Err(BackgroundRegistryError::ExternalCancelRequiresAsync(
                task_id.into(),
            ));
        }
        if let Some(task) = tasks.remove(task_id) {
            let needs_cleanup = task.kind == BgTaskKind::Shell
                || (task.kind == BgTaskKind::Agent
                    && matches!(task.cancel_handle, BgCancelHandle::Abort(_)));
            if needs_cleanup {
                self.cancelled_tasks_waiting_cleanup
                    .lock()
                    .insert(task_id.to_owned(), task.kind);
            } else {
                let ticket = self
                    .registrations
                    .lock()
                    .get(task_id)
                    .map(|receipt| receipt.ticket());
                if let Some(ticket) = ticket {
                    self.mark_registration_settled(&ticket);
                }
            }
            if let Some(inbox) = &task.agent_inbox {
                inbox.close();
            }
            match task.cancel_handle {
                BgCancelHandle::Abort(mut handle) => {
                    // S3.2：先触发工具层取消链——任务在下一个响应 cancel 的 await 点
                    // （reason LLM 调用 / 工具执行 / idle 等待）退出，走完整收尾
                    // （SubagentStopped / deregister / thread status / stop hooks）。
                    if let Some(token) = task.cancel_token.as_ref() {
                        token.cancel();
                    }
                    // 超时兜底：等待任务自然结束（grace 窗口内响应 cancel 则保留
                    // async 收尾），超时再 abort——否则"取消后任务继续跑"比 abort 更糟。
                    // abort 兜底路径：任务内同步收尾 guard（deregister_runtime 等）仍执行，
                    // async 收尾（update_thread_status / stop hooks）丢失并记日志。
                    match tokio::runtime::Handle::try_current() {
                        Ok(_) => {
                            let task_id_owned = task_id.to_string();
                            self.scope.spawn_admitted(async move {
                                let joined = match peri_time::timeout(
                                    std::time::Duration::from_secs(CANCEL_GRACE_SECS),
                                    &mut handle,
                                )
                                .await
                                {
                                    Ok(joined) => joined,
                                    Err(_) => {
                                        handle.abort();
                                        let joined = handle.await;
                                        warn!(
                                            task_id = %task_id_owned,
                                            "bg task cancel: grace period elapsed, aborted task \
                                             (async cleanup lost: thread status / stop hooks; \
                                             sync cleanup guard still runs)"
                                        );
                                        joined
                                    }
                                };
                                if let Err(error) = joined {
                                    if !error.is_cancelled() {
                                        warn!(
                                            task_id = %task_id_owned,
                                            is_panic = error.is_panic(),
                                            "bg task cancel: execution join failed"
                                        );
                                    }
                                }
                            });
                        }
                        Err(_) => {
                            // 无 tokio runtime 上下文（防御；生产调用点均在 async 上下文）：
                            // 无法异步等待，直接 abort 兜底。
                            handle.abort();
                            warn!(
                                task_id = %task_id,
                                "bg task cancel: no tokio runtime for graceful wait, aborted task"
                            );
                        }
                    }
                }
                BgCancelHandle::Kill(Some(kill)) => {
                    // 触发 kill 闭包：workflow 场景转发到 WorkflowTaskRegistry::kill
                    kill();
                }
                BgCancelHandle::Kill(None) => {
                    // 上方已校验，理论不可达；防御性保留
                    unreachable!("Kill(None) checked before task removal");
                }
                BgCancelHandle::External { .. } => {
                    unreachable!("external cancellation is asynchronous")
                }
            }
            let mut projection = self.projection.lock();
            if let Some(record) = projection.records.get_mut(task_id) {
                record.status = "cancelled".into();
            }
            if self.release_settled_registration(task_id) {
                projection.retire(task_id);
            }
            self.push_event(
                &mut projection,
                BgRegistryEvent::Cancelled {
                    task_id: task_id.to_string(),
                    reason: "user cancelled".to_string(),
                },
            );
            drop(projection);
            drop(tasks);
            self.notify_activity_change();

            Ok(())
        } else {
            Err(BackgroundRegistryError::TaskNotFound(task_id.to_string()))
        }
    }
}
