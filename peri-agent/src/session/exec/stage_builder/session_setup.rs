//! 主 Session 的冻结数据、持久化与 async owner 装配。
use super::StageBuildInput;
use crate::session::{exec::executor::FrozenSessionData, Session};
use peri_acp_types::{
    cron::CronSchedulerPort,
    session::{CronOwner, MessageQueue, SessionInbox},
};
use std::sync::Arc;

pub(super) fn build_session(
    input: &StageBuildInput,
    frozen_session: &FrozenSessionData,
    cwd: &str,
    session_id: &str,
    cancel_arc: &Arc<tokio_util::sync::CancellationToken>,
    shared_queue: &MessageQueue,
    cron_scheduler: &Option<Arc<dyn CronSchedulerPort>>,
) -> Arc<Session> {
    // 构造 v2 Session（复用外部 cancel token + 会话级共享 MessageQueue）
    let cwd_arc: Arc<str> = Arc::from(cwd);
    let frozen_ctx = frozen_session.v2_frozen().clone();
    let session = Session::new_with_cancel_and_queue(
        cwd_arc,
        frozen_ctx,
        None,
        cancel_arc.clone(),
        shared_queue.clone(),
    );

    // 激活 transcript persistence（compact flags 跨 prompt 持久化）
    if let (Some(store), Some(tid)) = (input.thread_store.as_ref(), input.thread_id.as_ref()) {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        *transcript = old.with_persistence(store.clone(), tid.clone());
    }

    // Async Owners（SessionInbox + CronOwner）
    //
    // Session 级路径（TUI/stdio 交互，存在 SessionManager）：cron bridge 由
    // SessionManager::cron_bridge_for 在 AcpSession 上懒启动，跨 turn 存活——
    // turn 结束（含 retry Error）不再杀死 bridge
    // （spec/issues/2026-08-04-cron-trigger-lost-after-turn-error.md）。
    // 此处不再挂载 turn 级 CronOwner，也不调用 set_async_owners
    // （AsyncOwners 容器无生产消费者；executor 的 idle_inbox 走 session 级 inbox）。
    //
    // 无 SessionManager 的路径（print 模式 -p，单次进程）：保留原 turn 级挂载，
    // 行为与现状完全一致。
    if input.launch_cron_bridge.is_some() {
        if let Some(ref launch) = input.launch_cron_bridge {
            launch(session_id);
        }
    } else if let Some(ref scheduler) = cron_scheduler {
        // ── 原 AsyncOwners 块原样保留（含 per-turn SessionInbox + subscribe +
        //    bridge task + CronOwner + set_async_owners）──
        {
            let shared_queue_arc = Arc::new(shared_queue.clone());
            let session_inbox = SessionInbox::new(shared_queue_arc);
            let inbox_handle = session_inbox.handle();

            let mut trigger_rx = scheduler.subscribe();

            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
            let shutdown = cancel_arc.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => {
                            break;
                        }
                        trigger = trigger_rx.recv() => {
                            match trigger {
                                Some(t) => {
                                    if prompt_tx.send(t.prompt).is_err() {
                                        tracing::debug!("cron-bridge: prompt_tx closed, stopping");
                                        break;
                                    }
                                }
                                None => {
                                    tracing::debug!("cron-bridge: trigger_rx closed, stopping");
                                    break;
                                }
                            }
                        }
                    }
                }
            });

            let mut owner = CronOwner::new();
            owner.start(prompt_rx, inbox_handle, cancel_arc.clone());
            tracing::info!("CronOwner started (ACP bridge path)");

            // 分支内 scheduler 恒为 Some（else-if 绑定），直接注入
            session.set_async_owners(session_inbox, Some(owner), None);
        }
    }

    // MCP 订阅 inbox 惰性注册（幂等；SessionManager 路径注册到订阅端口，
    // 无 SessionManager 时 no-op——print 模式不接收外部订阅通知唤醒）
    if let Some(ref launch) = input.launch_mcp_subscription {
        launch(session_id);
    }

    session
}
