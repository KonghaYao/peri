//! Shared user/continuation prompt dispatch under the per-session serialization lock.

use std::sync::Arc;

use super::continuation;
use crate::dispatch::prompt::extract_and_validate_run_prompt_params;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::thread::{ThreadId, WorkState};
use serde_json::Value;

use super::{extract_session_id, AcpServerConfig, PromptLocks, SharedSessions};
use crate::transport::types::AcpError;

#[derive(Clone, Copy)]
pub(crate) enum PromptOrigin {
    User,
    QueuedUser,
    Continuation {
        mq_steering: bool,
        /// 由「已规范提交但未覆盖的 Ready 结果」触发的续跑。
        pending_result: bool,
        /// 由恢复启动门发出：早失败要反馈给恢复协调器，供下一次显式进入重试。
        recovery: bool,
    },
    Scheduled,
}

/// 用户 prompt 与内部 AsyncContinuation 的**共享执行路径**。
///
/// 复用同一套：AgentPool 取出/归还、per-session prompt lock、run_prompt 后处理
/// （history 持久化 / cancel 回滚 / recall 回写）、prediction fork。continuation
/// 不发送 ACP response（无 request id），且不触发 prediction。
///
/// 用户 prompt 与当前运行态的内部续跑共享同一序列化入口。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_prompt_turn(
    params: Value,
    origin: PromptOrigin,
    continuation_epoch: Option<u64>,
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cfg: &AcpServerConfig,
    cont_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
) -> Result<Value, AcpError> {
    dispatch_prompt_turn_with_input(
        params,
        origin,
        continuation_epoch,
        sessions,
        prompt_locks,
        transport,
        cfg,
        cont_tx,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_prompt_turn_with_input(
    params: Value,
    origin: PromptOrigin,
    continuation_epoch: Option<u64>,
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cfg: &AcpServerConfig,
    cont_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
    input_ticket: Option<super::user_input::UserInputRun>,
) -> Result<Value, AcpError> {
    let is_continuation = !matches!(origin, PromptOrigin::User);
    // 恢复启动门发出的 continuation：只有它的早失败反馈给协调器。用户 prompt 或
    // 普通续跑失败不改变恢复准入。
    let recovery_origin = matches!(origin, PromptOrigin::Continuation { recovery: true, .. });
    let prompt_session_id = extract_session_id(&params, "").to_string();
    let environment = sessions
        .lock()
        .await
        .get(&prompt_session_id)
        .and_then(|state| state.environment.clone());
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    // 恢复准备失败且分类为不可执行时，服务端拒绝本会话的全部执行入口：
    // 用户 prompt、scheduled、continuation 与 idle-suspended 注入都在这里截住。
    if let Some(block) = cfg
        .session_manager
        .get_session(&prompt_session_id)
        .and_then(|session| session.recovery_execution_block())
    {
        return Err(AcpError::new(
            -32017,
            format!(
                "Session execution is blocked by a failed recovery: {}: {}",
                block.kind, block.detail
            ),
        )
        .with_data(serde_json::json!({
            "reason": "recovery_execution_blocked",
            "kind": block.kind.as_str(),
            "threadId": block.thread_id,
            "detail": block.detail,
        })));
    }
    {
        let sessions = sessions.lock().await;
        let state = sessions
            .get(&prompt_session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        if state.closing || state.close_in_flight > 0 {
            return Err(AcpError::new(-32010, "Session is closing"));
        }
        if cfg
            .session_manager
            .get_session(&prompt_session_id)
            .is_some_and(|session| session.cancel_token.is_cancelled())
        {
            return Err(AcpError::new(-32010, "Session runtime is no longer active"));
        }
    }
    // 等待 session 锁之前的先行检查：只复核已记录证据，让绑定已失效的提交立刻失败，
    // 而不是先排队等锁。本次准入的权威复核在取得锁之后（见下方 validate_expected）。
    super::workspace::reassert_expected(cfg, &prompt_session_id, None).await?;

    // 用户显式新 prompt 清掉未运行的 continuation（scheduler 的原子 take 与
    // epoch 校验保证不会重复/过期执行）。必须在等待 prompt lock 前递增代际，
    // 使已排队的 continuation 失效；continuation 自身仅在真正拿到锁后才标记
    // in_flight，避免其尚在排队时掩盖对原 prompt 的取消。
    if !is_continuation {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            state.continuation_armed = false;
            state.continuation_mq_steering_pending = false;
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
        super::workspace::validate_expected(cfg, &prompt_session_id, None).await?;
        let (_, content, _attachments) = extract_and_validate_run_prompt_params(&params)?;
        if let Some(inbox) = cfg.session_manager.session_inbox_for(&prompt_session_id) {
            if let Err(error) = inbox
                .handle()
                .push_prompt(
                    peri_acp_types::session::MessageSource::UserInput,
                    BaseMessage::human(content),
                )
                .await
            {
                return Err(AcpError::new(
                    -32603,
                    format!("prompt injection into suspended turn failed: {error}"),
                ));
            }
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

    // Serialize prompts per session: wait for any in-flight prompt to finish so the next
    // turn sees the adopted effective history (or ReloadRequired) instead of a HandedOff
    // empty container.
    let _guard = prompt_lock.lock().await;
    // 本次准入发布的令牌。未移交执行器就返回（错误、取消、attach 失败等）时只撤销
    // 这一个；后续轮次的令牌可能已被替换，按身份比较避免误清（#196 P2 复审）。
    let mut admitted_cancel: Option<tokio_util::sync::CancellationToken> = None;
    {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&prompt_session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        if state.closing || state.close_in_flight > 0 {
            return Err(AcpError::new(-32010, "Session is closing"));
        }
        if cfg
            .session_manager
            .get_session(&prompt_session_id)
            .is_some_and(|session| session.cancel_token.is_cancelled())
        {
            return Err(AcpError::new(-32010, "Session runtime is no longer active"));
        }
        // 准入闸门与令牌发布必须同锁：close 先置闸门再取消令牌，若令牌还没发布
        // 就无从取消，已准入的轮次会在关闭等锁期间开跑并把 close 拖到超时
        // （#196 P2）。epoch 入口（续跑 / 定时）在下方提交段原子发布令牌；
        // 用户 prompt 与队列输入在这里发布。
        if continuation_epoch.is_none() {
            let token = tokio_util::sync::CancellationToken::new();
            state.cancel_token = Some(token.clone());
            admitted_cancel = Some(token);
        }
    }
    #[cfg(test)]
    admission_gate::wait(&format!("{prompt_session_id}:admitted")).await;
    if let Err(error) = super::workspace::validate_expected(cfg, &prompt_session_id, None).await {
        rollback_turn_admission(
            sessions,
            &prompt_session_id,
            is_continuation,
            admitted_cancel.as_ref(),
        )
        .await;
        return Err(error);
    }
    if let Some(epoch) = continuation_epoch {
        #[cfg(test)]
        admission_gate::wait(&prompt_session_id).await;
        let mq_steering = matches!(
            origin,
            PromptOrigin::Scheduled
                | PromptOrigin::Continuation {
                    mq_steering: true,
                    ..
                }
        );
        // 只有显式的 continuation mq steering 需要激活门；Scheduled 已获调度批准，
        // 不能再用上一次失败留下的抑制状态拒绝它。
        let continuation_mq_steering = matches!(
            origin,
            PromptOrigin::Continuation {
                mq_steering: true,
                ..
            }
        );
        let pending_result = matches!(
            origin,
            PromptOrigin::Continuation {
                pending_result: true,
                ..
            }
        );
        // MQ 激活查询不能持有 sessions 锁。查询错误不能折叠成「没有工作」：
        // 二次检查只取决策需要的事实，Err 时在预算内重查，耗尽后交还 listener。
        let queue = cfg
            .session_manager
            .get_session(&prompt_session_id)
            .map(|session| session.v2_message_queue.clone());
        let subagent_complete = peri_acp_types::session::MessageSource::SubAgentComplete;
        // 只有非 mq、非 pending_result 的 continuation 消费一次性 armed；
        // Scheduled 走同一查询分支但从未 take，不能借失败恢复出资格。
        let armed_continuation = matches!(
            origin,
            PromptOrigin::Continuation {
                mq_steering: false,
                pending_result: false,
                ..
            }
        );
        // 同一轮二次检查共享一个预算：多项查询的等待时间不能叠加放大。
        let recheck_deadline =
            tokio::time::Instant::now() + super::activation::DISPATCH_RECHECK_BUDGET;
        let has_subagent = if !mq_steering && !pending_result {
            match queue.as_ref() {
                Some(queue) => match super::activation::checked_dispatch_query(
                    &prompt_session_id,
                    "subagent_defer",
                    recheck_deadline,
                    || queue.has_pending_defer(&subagent_complete),
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        // scheduler 的 take 已消费一次性 armed。查询无法回答不等于
                        // Defer 不存在；先把资格还给同一代际，再唤醒 listener，
                        // 避免 listener 先醒后睡把结果留到下一次用户输入。
                        if armed_continuation {
                            if let Some(epoch) = continuation_epoch {
                                let mut guard = sessions.lock().await;
                                let restored =
                                    guard.get_mut(&prompt_session_id).is_some_and(|state| {
                                        continuation::restore_continuation_armed(state, epoch)
                                    });
                                drop(guard);
                                tracing::warn!(
                                    session_id = %prompt_session_id,
                                    epoch,
                                    query = "subagent_defer",
                                    restored,
                                    "continuation armed restore after dispatch query failure"
                                );
                            }
                        }
                        return Ok(abort_continuation_query_error(
                            cfg,
                            &prompt_session_id,
                            "subagent_defer",
                            &error,
                            recovery_origin,
                        ));
                    }
                },
                None => false,
            }
        } else {
            false
        };
        let has_mq = if mq_steering {
            match queue.as_ref() {
                Some(queue) => match super::activation::checked_dispatch_query(
                    &prompt_session_id,
                    "mq_continuation",
                    recheck_deadline,
                    || queue.needs_mq_continuation(),
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        return Ok(abort_continuation_query_error(
                            cfg,
                            &prompt_session_id,
                            "mq_continuation",
                            &error,
                            recovery_origin,
                        ));
                    }
                },
                None => false,
            }
        } else {
            false
        };

        // 重查可能占用至预算上限：取消、Stop 与 Ready 结果按重查后的事实判定，
        // 避免把重查窗口内的新事实当成旧快照。
        let (cancelled, user_stopped, has_ready_result) =
            match cfg.session_manager.get_session(&prompt_session_id) {
                Some(session) => (
                    session.cancel_token.is_cancelled(),
                    session.activation.user_stopped(),
                    session.async_result_mailbox.has_ready(),
                ),
                None => (true, false, false),
            };

        // 定时调度是已批准的调度，不受上次失败的水位影响；但它是被动回调，
        // 不能解除用户显式 Stop（设计 §2.1/§5.2）。否则准入会写 working 覆盖 stopped。
        if matches!(origin, PromptOrigin::Scheduled) && user_stopped {
            tracing::debug!(
                session_id = %prompt_session_id,
                "scheduled continuation: user stop is still in effect, aborting"
            );
            return Ok(serde_json::Value::Null);
        }
        let base_dispatchable = {
            let sessions = sessions.lock().await;
            sessions.get(&prompt_session_id).is_some_and(|state| {
                if state.closing || cancelled {
                    return false;
                }
                continuation::continuation_dispatchable(
                    state,
                    epoch,
                    has_subagent,
                    has_mq,
                    mq_steering,
                    pending_result,
                    has_ready_result,
                )
            })
        };
        let mq_allowed = if continuation_mq_steering {
            match super::activation::checked_dispatch_query(
                &prompt_session_id,
                "mq_steering_allowed",
                recheck_deadline,
                || super::activation::mq_steering_allowed_checked(cfg, &prompt_session_id),
            )
            .await
            {
                Ok(allowed) => allowed,
                Err(error) => {
                    return Ok(abort_continuation_query_error(
                        cfg,
                        &prompt_session_id,
                        "mq_steering_allowed",
                        &error,
                        recovery_origin,
                    ));
                }
            }
        } else {
            // 非 mq steering 不消费该判定，也不应因无关查询失败而中止。
            true
        };
        if !base_dispatchable
            || (continuation_mq_steering && !mq_allowed)
            || (pending_result && !super::activation::result_allowed(cfg, &prompt_session_id).await)
        {
            tracing::debug!(
                session_id = %prompt_session_id,
                "continuation: superseded (newer prompt or Defer consumed), aborting"
            );
            if recovery_origin {
                mark_recovery_launch_failed(cfg, &prompt_session_id);
            }
            return Ok(serde_json::Value::Null);
        }
        let dispatchable = {
            let mut sessions = sessions.lock().await;
            sessions.get_mut(&prompt_session_id).is_some_and(|state| {
                if state.closing || state.close_in_flight > 0 {
                    return false;
                }
                state.continuation_in_flight = true;
                state.continuation_mq_steering_pending = false;
                let token = tokio_util::sync::CancellationToken::new();
                state.cancel_token = Some(token.clone());
                admitted_cancel = Some(token);
                true
            })
        };
        if !dispatchable {
            if recovery_origin {
                mark_recovery_launch_failed(cfg, &prompt_session_id);
            }
            return Ok(serde_json::Value::Null);
        }
        #[cfg(test)]
        admission_gate::wait(&format!("{prompt_session_id}:committed")).await;
    }

    // 准入已通过、执行尚未开始：先提交 durable 工作意图。进程若在这一刻退出，
    // 下次进入仍能看出这一轮需要继续；提交失败必须拒绝本轮，不能在没有续跑意图
    // 证据的情况下放行模型调用。
    let thread_id = {
        let sessions = sessions.lock().await;
        sessions
            .get(&prompt_session_id)
            .map(|state| state.thread_id.clone())
    };
    let Some(thread_id) = thread_id else {
        rollback_turn_admission(
            sessions,
            &prompt_session_id,
            is_continuation,
            admitted_cancel.as_ref(),
        )
        .await;
        return Err(work_intent_error(
            None,
            "session thread identity is missing".to_owned(),
        ));
    };
    let thread = ThreadId::from(thread_id.clone());
    // 恢复 child 的父级运行登记必须发生在准入之后、模型调用之前：只有真正获得
    // 本次 turn 资格的 child 才进入父任务投影。恢复提醒可能先被激活监听拉起
    // （`recovery_origin=false`），因此按投递事实而不是启动来源判定；登记失败
    // 撤销启动资格，提醒与准入留给下一次显式进入重试。
    if is_continuation {
        if let Err(error) =
            super::recovery::register_recovered_child_for_turn(cfg, sessions, &prompt_session_id)
                .await
        {
            rollback_turn_admission(
                sessions,
                &prompt_session_id,
                is_continuation,
                admitted_cancel.as_ref(),
            )
            .await;
            if recovery_origin {
                mark_recovery_launch_failed(cfg, &prompt_session_id);
            }
            tracing::warn!(
                session_id = %prompt_session_id,
                %error,
                "recovery: parent task registration failed; launch rolled back"
            );
            return Ok(serde_json::Value::Null);
        }
    }
    if let Err(error) = cfg
        .session_manager
        .session_resources()
        .set_work_state(&thread, WorkState::Working)
        .await
    {
        rollback_turn_admission(
            sessions,
            &prompt_session_id,
            is_continuation,
            admitted_cancel.as_ref(),
        )
        .await;
        // 已登记但尚未开始执行的父任务必须收尾；没有投递时是 no-op。
        super::recovery::abort_recovery_launch(cfg, &prompt_session_id);
        if recovery_origin {
            mark_recovery_launch_failed(cfg, &prompt_session_id);
        }
        tracing::error!(
            session_id = %prompt_session_id,
            %error,
            "work state write before prompt failed; prompt rejected"
        );
        return Err(work_intent_error(Some(&thread_id), error.to_string()));
    }

    // 冷 child 恢复的本轮把终态、工作意图与结果消息交给一次原子结算（§5.3）：
    // 在 run_prompt 前标记，finish_prompt_turn 据此跳过独立的 work_state 写入。
    let recovery_parent = cfg
        .session_manager
        .get_session(&prompt_session_id)
        .and_then(|runtime| runtime.recovery_parent());
    if recovery_parent.is_some() {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            state.recovery_settlement_pending = true;
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
        let pool = match sessions.get_mut(&prompt_session_id) {
            Some(state) => std::mem::take(&mut state.agent_pool),
            None => Default::default(),
        };
        Arc::new(parking_lot::Mutex::new(pool))
    };

    let outcome = super::prompt::run_prompt_with_outcome(
        params,
        sessions,
        cfg,
        transport,
        pool_arc.clone(),
        Some(cont_tx.clone()),
        is_continuation,
        input_ticket,
    )
    .await;
    let (result, prediction_execution) = match outcome {
        Ok(outcome) => (Ok(outcome.response), outcome.execution_binding),
        Err(error) => (Err(error), None),
    };

    // `run_prompt` 在 attach 前失败时返回 `Ok(Null)`：本轮从未移交执行器，成功空操作
    // 同样要撤销本次准入发布的令牌，否则 close 会把空轮次误判为在飞（#196 P2 复审）。
    if matches!(&result, Ok(Value::Null)) {
        rollback_turn_admission(
            sessions,
            &prompt_session_id,
            is_continuation,
            admitted_cancel.as_ref(),
        )
        .await;
    }

    // Prediction remains admitted before pool restoration and while the prompt lock is held.
    if !is_continuation && result.is_ok() {
        super::prediction::spawn_prediction(
            transport,
            &prompt_session_id,
            sessions,
            cfg,
            prediction_execution,
        );
    }

    let mut abandoned_thread = None;
    let mut recovery_early_failure = false;
    {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            let recovery_settlement_pending =
                std::mem::take(&mut state.recovery_settlement_pending);
            state.continuation_in_flight = false;
            if result.is_err() {
                // Early assembly/controller errors may precede finish_prompt_turn.
                // The prompt lock still identifies this attempt as the sole writer.
                // 只有还能取到本轮令牌才说明本轮从未收尾：否则错误只是收尾后的
                // wire 投影，工作意图已经写好（可能是 stopped），不得覆盖。
                // 恢复结算由 finalize 统一写终态与工作意图，这里不能再单独写 idle。
                let never_finished = admitted_cancel
                    .as_ref()
                    .is_some_and(|token| state.cancel_token.as_ref() == Some(token));
                if never_finished {
                    state.cancel_token = None;
                }
                // 恢复来源的本轮早失败：既可能是冷 child 的恢复结算，也可能是
                // 根/child 的恢复启动 continuation。保留 working 与提醒，供下一次
                // 显式进入重试；不能按普通中断回收成 idle。
                let recovery_attempt = recovery_settlement_pending || recovery_origin;
                if never_finished && recovery_attempt {
                    recovery_early_failure = true;
                } else if never_finished {
                    abandoned_thread = Some(state.thread_id.clone());
                }
            }
            if let Ok(mutex) = Arc::try_unwrap(pool_arc) {
                state.agent_pool = mutex.into_inner();
            }
        }
    }
    // 恢复 turn 在进入收尾前明确失败：本轮没有产生可结算的终态。按设计 §5.1
    // 不伪造完成或取消，保留 `working` 与原提醒供下一次显式进入重试；同时把当前
    // MQ 水位记为失败水位，避免遗留提醒立刻触发第二次自动尝试。
    if recovery_early_failure {
        // 进程内事实：本次恢复启动没有跑到 Receive。下一次显式进入读取并清除，
        // 允许采纳同一提醒重试；此处不做任何自动重试。
        let runtime = cfg.session_manager.get_session(&prompt_session_id);
        if let Some(runtime) = runtime {
            runtime.mark_recovery_launch_failed();
            let queue = runtime.v2_message_queue.clone();
            let activation = Arc::clone(&runtime.activation);
            drop(runtime);
            let watermark = queue.admission_watermark().await.ok().flatten();
            activation.record_failed_attempt(watermark);
        }
    }
    // 准入后从未进入收尾的尝试按「明确失败」回收工作意图，避免 `working` 无限期滞留。
    // 回收同样走收尾口径：仍需处理的新消息留在 MQ 时保持 `working`，显式停止不被解除，
    // 收尾阶段 turn 已经结束，失败只能重试并显式告警，不能伪装成成功。
    if let Some(thread_id) = abandoned_thread {
        if let Err(error) = settle_work_state_with_retry(
            cfg.session_manager.session_resources(),
            &ThreadId::from(thread_id),
        )
        .await
        {
            tracing::error!(
                session_id = %prompt_session_id,
                %error,
                "work state recovery after aborted prompt failed"
            );
        }
    }

    // 冷 child 恢复的 turn 结束：终态、工作意图与结果通知由一次原子结算提交到父
    // thread。目标挂在会话实例上，所以用户 prompt、激活监听与启动门任一路径跑完都只
    // 结算一次；被取代的续跑在本函数早期返回，走不到这里。早失败没有终态可结算，
    // 不向父级投递「已完成/已失败」的结果事实，但仍要收尾已登记的父级任务。
    let recovery_delivery = cfg
        .session_manager
        .get_session(&prompt_session_id)
        .and_then(|runtime| runtime.take_recovery_parent());
    if let Some(delivery) = recovery_delivery {
        if recovery_early_failure {
            super::recovery::finish_recovered_child_without_result(cfg, &delivery);
        } else {
            super::recovery::deliver_child_turn(
                cfg,
                sessions,
                &prompt_session_id,
                &delivery,
                super::recovery::turn_outcome(&result),
            )
            .await;
        }
    }

    let pending_state = {
        let mut sessions = sessions.lock().await;
        sessions.get_mut(&prompt_session_id).map(|state| {
            state.continuation_mq_steering_pending = false;
            super::activation::PendingState::from_state(state)
        })
    };
    let continuation = match (
        pending_state,
        cfg.session_manager.get_session(&prompt_session_id),
    ) {
        (Some(state), Some(runtime)) => {
            let queue = runtime.v2_message_queue.clone();
            let activation = Arc::clone(&runtime.activation);
            let mailbox = Arc::clone(&runtime.async_result_mailbox);
            drop(runtime);
            let pending = super::activation::pending_request(
                &prompt_session_id,
                &state,
                &queue,
                &activation,
                &mailbox,
            )
            .await;
            Some((pending, queue))
        }
        _ => None,
    };
    match continuation {
        Some((super::activation::PendingRequest::Ready(request), _)) => {
            if cont_tx.send(request).is_err() {
                tracing::error!(session_id = %prompt_session_id, "continuation scheduling failed: channel closed");
            }
        }
        Some((super::activation::PendingRequest::Retry(error), queue)) => {
            // 本轮收尾后的查询失败同样不能消耗唤醒：把重查交还 listener。
            tracing::warn!(
                session_id = %prompt_session_id,
                %error,
                "continuation pending check failed; requesting listener recheck"
            );
            queue.request_recheck();
        }
        Some((super::activation::PendingRequest::Idle, _)) | None => {}
    }
    result
}

/// 准入前工作意图提交失败：确定性拒绝，不进入模型调用。
fn work_intent_error(thread_id: Option<&str>, detail: String) -> AcpError {
    AcpError::new(
        -32603,
        format!("work state write before prompt failed: {detail}"),
    )
    .with_data(serde_json::json!({
        "reason": "work_intent_commit_failed",
        "threadId": thread_id,
        "detail": detail,
    }))
}

/// 恢复启动的 continuation 在 Receive 前失败：写进程内标记，下一次显式进入
/// 据此清除 Launched 占位并重新采纳提醒。用户 prompt 与非恢复续跑不写。
fn mark_recovery_launch_failed(cfg: &AcpServerConfig, session_id: &str) {
    if let Some(runtime) = cfg.session_manager.get_session(session_id) {
        runtime.mark_recovery_launch_failed();
    }
}

/// 二次资格查询在预算内仍无法回答：保留重查责任后放弃本轮。
///
/// 查询错误不等于「没有工作」；一次瞬时存储故障不能消耗掉已消费的唤醒。
/// 返回 `Null` 与既有 superseded 跳过路径一致，区别是 listener 会被重新唤醒。
fn abort_continuation_query_error(
    cfg: &AcpServerConfig,
    session_id: &str,
    query: &'static str,
    error: &str,
    recovery_origin: bool,
) -> Value {
    tracing::warn!(
        session_id = %session_id,
        query,
        %error,
        retry_budget_ms = super::activation::DISPATCH_RECHECK_BUDGET.as_millis() as u64,
        "continuation eligibility query failed; requesting listener recheck"
    );
    if let Some(queue) = cfg.session_manager.v2_queue_for(session_id) {
        queue.request_recheck();
    }
    if recovery_origin {
        mark_recovery_launch_failed(cfg, session_id);
    }
    Value::Null
}

/// 回滚本轮在准入段发布的内存标记。
///
/// 用户 prompt 与队列输入在准入段发布 `cancel_token`（与关闭闸门同锁），续跑
/// 入口额外持有 `continuation_in_flight`。任何在 `run_prompt` 之前的确定性失败
/// 都必须在释放 prompt lock 前撤销，否则关闭/恢复会把从未开跑的轮次误判为在飞。
/// 只撤销 `admitted_cancel` 指定的令牌：令牌可能已被后续轮次替换，按身份比较
/// 避免误清（#196 P2 复审）。
async fn rollback_turn_admission(
    sessions: &SharedSessions,
    session_id: &str,
    is_continuation: bool,
    admitted_cancel: Option<&tokio_util::sync::CancellationToken>,
) {
    let mut sessions = sessions.lock().await;
    if let Some(state) = sessions.get_mut(session_id) {
        if admitted_cancel.is_some_and(|token| state.cancel_token.as_ref() == Some(token)) {
            state.cancel_token = None;
        }
        if is_continuation {
            state.continuation_in_flight = false;
        }
    }
}

/// 收尾写入的有限重试：turn 已执行并提交正文，失败不能再拒绝执行，只能重试一次
/// 并让调用方记录可诊断错误。
pub(super) async fn set_work_state_with_retry(
    resources: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    thread_id: &ThreadId,
    state: WorkState,
) -> Result<(), String> {
    match resources.set_work_state(thread_id, state).await {
        Ok(()) => Ok(()),
        Err(first) => match resources.set_work_state(thread_id, state).await {
            Ok(()) => Ok(()),
            Err(second) => Err(format!("{second} (first attempt: {first})")),
        },
    }
}

/// [`SessionResources::settle_work_state_after_turn`] 的有限重试；语义同
/// [`set_work_state_with_retry`]，返回是否真的写入 `idle`。
pub(super) async fn settle_work_state_with_retry(
    resources: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    thread_id: &ThreadId,
) -> Result<bool, String> {
    match resources.settle_work_state_after_turn(thread_id).await {
        Ok(written) => Ok(written),
        Err(first) => match resources.settle_work_state_after_turn(thread_id).await {
            Ok(written) => Ok(written),
            Err(second) => Err(format!("{second} (first attempt: {first})")),
        },
    }
}

#[cfg(test)]
pub(crate) mod admission_gate {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};
    use tokio::sync::Notify;

    #[derive(Default)]
    pub(crate) struct Gate {
        pub(crate) entered: Notify,
        pub(crate) release: Notify,
    }

    static GATES: LazyLock<Mutex<HashMap<String, Arc<Gate>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) fn install(session_id: &str) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        GATES
            .lock()
            .unwrap()
            .insert(session_id.into(), gate.clone());
        gate
    }

    pub(in crate::host) async fn wait(session_id: &str) {
        let gate = GATES.lock().unwrap().remove(session_id);
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
}
