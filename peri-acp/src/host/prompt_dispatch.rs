//! Shared user/continuation prompt dispatch under the per-session serialization lock.

use std::sync::Arc;

use peri_acp_types::messages::BaseMessage;
use serde_json::Value;

use super::{
    continuation, extract_session_id, run_prompt, AcpServerConfig, PromptLocks, SharedSessions,
};
use crate::dispatch::prompt::extract_and_validate_run_prompt_params;
use crate::transport::types::AcpError;

/// 用户 prompt 与内部 AsyncContinuation 的**共享执行路径**。
///
/// 复用同一套：AgentPool 取出/归还、per-session prompt lock、run_prompt 后处理
/// （history 持久化 / cancel 回滚 / recall 回写）、prediction fork。continuation
/// 不发送 ACP response（无 request id），且不触发 prediction。
///
/// 用户显式新 prompt 会清除未运行的 continuation：置位前先
/// `continuation_armed = false` 并递增 `continuation_epoch`（scheduler 在
/// 获取 prompt lock 后校验代际，见 continuation.rs）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_prompt_turn(
    params: Value,
    is_continuation: bool,
    continuation_epoch: Option<u64>,
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cfg: &AcpServerConfig,
    cont_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
) -> Result<Value, AcpError> {
    let prompt_session_id = extract_session_id(&params, "").to_string();

    // 多读者 + 单 writer lease：prompt 是写入操作，仅 writer 可提交。
    // 协议无客户端身份字段，writer 恒为 session 创建方（"default"）——
    // 未来引入 clientId 后此处按请求方判定即可（见 lease 模块文档）。
    {
        let sessions = sessions.lock().await;
        if let Some(state) = sessions.get(&prompt_session_id) {
            if !state.lease.is_writer("default") {
                return Err(AcpError::new(
                    -32603,
                    "read-only observer cannot submit prompt",
                ));
            }
        }
    }

    // 用户显式新 prompt 清掉未运行的 continuation（scheduler 的原子 take 与
    // epoch 校验保证不会重复/过期执行）。必须在等待 prompt lock 前递增代际，
    // 使已排队的 continuation 失效；continuation 自身仅在真正拿到锁后才标记
    // in_flight，避免其尚在排队时掩盖对原 prompt 的取消。
    if !is_continuation {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            state.continuation_armed = false;
            state.continuation_epoch += 1;
        }
    }

    // 挂起注入：session 当前在 await_wake 挂起（turn 在途但 idle，通常因 bg
    // 任务活跃——executor 在 run_react_loop 挂起期间置 idle_suspended 标志）。
    // 若在此等待 per-session prompt lock，注入会阻塞至当前 turn 完成——bg 任务
    // 可能长达数分钟，用户输入表现为"nothing happen"（TUI 侧 submit_consumer
    // 串行 await prompt RPC，被挂起的 RPC 卡住，后续提交全部排队）。
    // 正确语义：直接把用户消息推入 session inbox（Prompt + wake），挂起的
    // run_react_loop 醒来后由 Receive drain_all 消费，在**同一 turn** 内继续。
    // 注入后立即返回——当前 turn 的 TurnDone 会携带原 request_id（挂起时
    // 该 turn 已在执行），TUI 侧仅用 request_id 做 stale TurnInterrupted 配对，
    // TurnDone 路径不比对（见 peri-tui acp_events/turn.rs）。
    // NOTE: 此分支仅处理用户 prompt（is_continuation=false）。prompt_with_bg_results
    // 的 bgResults 在 run_session_loop 内 push Defer——挂起注入路径不携带
    // bgResults（该 RPC 仅 stdio 会话使用，allow_await_wake=false 永不挂起）。
    if !is_continuation && cfg.session_manager.is_idle_suspended(&prompt_session_id) {
        let (_, content, _attachments) = extract_and_validate_run_prompt_params(&params)?;
        if let Some(inbox) = cfg.session_manager.session_inbox_for(&prompt_session_id) {
            inbox.handle().push_prompt(
                peri_acp_types::session::MessageSource::UserInput,
                BaseMessage::human(content),
            );
            tracing::info!(
                session_id = %prompt_session_id,
                "prompt injected while turn suspended (await_wake); loop will wake and consume"
            );
            return Ok(serde_json::json!({}));
        }
    }

    let prompt_lock = {
        let mut locks = prompt_locks.lock().await;
        locks
            .entry(prompt_session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Serialize prompts per session: wait for any in-flight prompt to finish
    // so that state.history is up-to-date when this prompt reads it.
    let _guard = prompt_lock.lock().await;

    // AsyncContinuation 与用户 prompt 竞争时，必须在持有同一 prompt lock 后
    // 校验代际与 pending callback：此时不会与 Receive 的 drain_all 并发，确认
    // 的 Defer 会由随后的 continuation 消费。无 callback 则不构建 agent 空跑。
    if let Some(epoch) = continuation_epoch {
        let dispatchable = {
            let sessions = sessions.lock().await;
            sessions.get(&prompt_session_id).is_some_and(|state| {
                let (has_subagent, has_mq) = cfg
                    .session_manager
                    .get_session(&prompt_session_id)
                    .map(|session| {
                        (
                            session.v2_message_queue.has_pending_defer(
                                &peri_acp_types::session::MessageSource::SubAgentComplete,
                            ),
                            session.v2_message_queue.needs_mq_continuation(),
                        )
                    })
                    .unwrap_or((false, false));
                continuation::continuation_dispatchable(state, epoch, has_subagent, has_mq)
            })
        };
        if !dispatchable {
            tracing::debug!(
                session_id = %prompt_session_id,
                "continuation: superseded (newer prompt or Defer consumed), aborting"
            );
            return Ok(serde_json::Value::Null);
        }
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            state.continuation_in_flight = true;
            state.continuation_mq_steering_pending = false;
        }
    }

    // Extract AgentPool from session, wrap in Arc<Mutex> for
    // in-place modification inside executor.
    //
    // 取出必须在 prompt lock 之内：continuation 与用户 prompt 共用同一把
    // per-session 锁，若在锁外取出，并发的用户 prompt 会取走被 `mem::replace`
    // 换出的空池并先行归还，导致两轮共享同一缓存的两个池实例互相覆盖、
    // 缓存丢失（跨轮次热缓存是本池的核心价值）。归还仍在锁内（函数末尾）。
    let pool_arc = {
        let mut sessions = sessions.lock().await;
        let pool = sessions
            .get_mut(&prompt_session_id)
            .map(|s| std::mem::take(&mut s.agent_pool))
            .unwrap_or_default();
        Arc::new(parking_lot::Mutex::new(pool))
    };

    let result = run_prompt(
        params,
        sessions,
        cfg,
        transport,
        pool_arc.clone(),
        Some(cont_tx.clone()),
        is_continuation,
    )
    .await;

    // Prediction remains admitted before pool restoration and while the prompt lock is held.
    if !is_continuation && result.is_ok() {
        super::prediction::spawn_prediction(transport, &prompt_session_id, sessions, cfg);
    }

    // Restore AgentPool back into session (still inside the per-session prompt
    // lock — see the take-out comment above) and clear the continuation in-flight
    // marker. Both writes are unconditional after run_prompt returns, so every
    // non-panic path restores the pool and clears the marker.
    let mq_steering_reschedule = {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            if let Ok(mutex) = Arc::try_unwrap(pool_arc) {
                state.agent_pool = mutex.into_inner();
            }
            state.continuation_in_flight = false;
            let pending = state.continuation_mq_steering_pending;
            let needs_mq = cfg
                .session_manager
                .get_session(&prompt_session_id)
                .map(|session| session.v2_message_queue.needs_mq_continuation())
                .unwrap_or(false);
            if pending && !needs_mq {
                state.continuation_mq_steering_pending = false;
            }
            pending && needs_mq
        } else {
            false
        }
    };
    if mq_steering_reschedule {
        let _ = cont_tx.send(crate::session::executor::ContinuationRequest {
            session_id: prompt_session_id.clone(),
            kind: peri_acp_types::tasks::BgTaskKind::Agent,
            mq_steering: true,
        });
        tracing::debug!(
            session_id = %prompt_session_id,
            "continuation: rescheduled MQ steering after in-flight turn ended"
        );
    }

    result
}
