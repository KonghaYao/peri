use std::sync::Arc;

use peri_acp_types::{
    command::PromptStopReason,
    event::{EventSink, EventSubscriber, ExecutorEvent, SubscriptionError},
};
use tokio::sync::oneshot;
use tracing::debug;

/// Langfuse trace 收尾闭包（构造 JoinHandle 后由 pump 在 pump_done 之后 drop）。
pub type LangfuseEndFn =
    Arc<dyn Fn(Option<String>) -> Option<tokio::task::JoinHandle<()>> + Send + Sync>;

// ── Spawn Pump Request parameter object ─────────────────────────────────────

/// 事件泵启动请求（参数对象）。
pub struct SpawnPumpRequest {
    /// 事件订阅端口（ACP 适配层包装 Controller 订阅；泵消费广播并按
    /// session_id 过滤——事件三层化出口：发射点统一经
    /// `EventPublisher`，泵经 [`EventSubscriber::recv`] 消费）。
    pub subscription: Box<dyn EventSubscriber>,
    /// 事件发射点集合的关闭信号：所有发射点（forwarder / v1 直发）结束、
    /// `event_tx` 全部 drop 时触发（`closed()`），泵随后 drain 广播在途事件。
    pub event_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutorEvent>,
    pub stop_reason_rx: oneshot::Receiver<PromptStopReason>,
    pub sink: Arc<dyn EventSink>,
    pub session_id: String,
    pub effective_context_window: u32,
    /// Langfuse trace 启动闭包（L5：ACP 侧捕获 tracer + 本轮输入，pump 任务
    /// 开头调用 `on_turn_start`——trace 语义与迁移前一致）。
    pub langfuse_on_turn_start: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Langfuse trace 收尾闭包（L5：ACP 侧捕获 tracer，构造 JoinHandle 后
    /// 由调用方在 pump_done 之后 drop——fire-and-forget，不得阻塞管线）。
    pub langfuse_on_turn_end: Option<LangfuseEndFn>,
    /// 本轮 prompt 的 requestId——push_done 时透传回带（TUI stale 判定用）。
    pub request_id: Option<String>,
}

/// 后台事件泵句柄，通过 oneshot channel 与 pump_done_rx 配对。
pub struct PumpHandle {
    pub pump_done_rx: oneshot::Receiver<()>,
}

/// 单条事件的处理（Langfuse error_kind 捕获 / bg callback unstable 事件 /
/// 协议化推送）。事件循环与 drain 分支共用，保证两条路径处理语义一致。
async fn pump_process(
    exec_event: &ExecutorEvent,
    last_error: &mut Option<String>,
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    effective_context_window: u32,
) {
    // Capture error_kind from TurnEnded for on_turn_end at pump tail
    if let ExecutorEvent::TurnEnded { error_kind, .. } = exec_event {
        *last_error = error_kind.as_ref().map(|k| format!("{:?}", k));
    }

    // 4. bg callback: MessageAdded → TUI flush-then-push.
    //    agent ReAct 循环在消费 MQ Defer 消息时通过 EventBus 发出
    //    SyntheticUserMessage → mapper → ExecutorEvent::MessageAdded。
    //    TUI bridge 收到 BgCallbackBubble 后会先 flush current_turn 到
    //    committed，再 push bg callback，把同一轮 TurnDone 的 AI 内容
    //    分割为「bg 前」和「bg 后」两段。
    if matches!(exec_event, ExecutorEvent::MessageAdded(_)) {
        if let ExecutorEvent::MessageAdded(msg) = exec_event {
            let text = msg.content();
            sink.push_unstable_event(
                session_id,
                "bg-callback-user-message".to_string(),
                serde_json::json!({ "text": text }),
            )
            .await;
        }
    }

    sink.push_event(session_id, exec_event, effective_context_window)
        .await;
}

/// 启动主事件泵任务。
///
/// 任务循环（事件三层化：发射点 → [`EventPublisher`] → 本泵订阅）：
/// 1. trace_start → 订阅事件流（按 session_id 过滤）→ 协议化推送 sink
/// 2. 发射点全部结束（`event_rx.closed()`）→ drain 广播在途事件 → 退出事件循环
/// 3. trace_end + push_done → signal pump completion（在 Langfuse flush 之前）
/// 4. Langfuse flush（fire-and-forget，不得阻塞管线）
pub fn spawn_event_pump(req: SpawnPumpRequest) -> PumpHandle {
    let SpawnPumpRequest {
        mut subscription,
        mut event_rx,
        stop_reason_rx,
        sink,
        session_id,
        effective_context_window,
        langfuse_on_turn_start,
        langfuse_on_turn_end,
        request_id,
    } = req;

    let (pump_done_tx, pump_done_rx) = oneshot::channel();

    if langfuse_on_turn_end.is_some() {
        debug!(session_id = %session_id, "Langfuse tracer received for turn");
    }

    tokio::spawn(async move {
        // Start Langfuse trace
        if let Some(ref f) = langfuse_on_turn_start {
            f();
        }

        let mut last_error: Option<String> = None;

        loop {
            tokio::select! {
                biased;
                msg = subscription.recv() => {
                    match msg {
                        Ok(m) => {
                            // 订阅是全局广播：只处理本 session 的事件
                            if m.envelope.session_id == session_id {
                                if let Some(ev) = m.event {
                                    pump_process(
                                        &ev, &mut last_error, &sink, &session_id,
                                        effective_context_window,
                                    ).await;
                                }
                            }
                        }
                        Err(SubscriptionError::Lagged(n)) => {
                            tracing::warn!(n, "event subscription lagged, events dropped");
                        }
                        Err(SubscriptionError::Closed) => break,
                    }
                }
                ev = event_rx.recv() => {
                    match ev {
                        // 防御分支：event_tx 的遗留直发（如有遗漏的发送点）
                        Some(exec_event) => {
                            pump_process(
                                &exec_event, &mut last_error, &sink, &session_id,
                                effective_context_window,
                            ).await;
                        }
                        None => {
                            // 发射点集合全部结束（event_tx 全 drop）：drain 广播中
                            // 已入 buffer 的在途事件（broadcast send 同步入 buffer，
                            // 关闭后到达的事件与 pump 退出后的丢弃语义一致——
                            // 对应迁移前 close_channel 后 forwarder 检查 None 丢弃）。
                            loop {
                                match subscription.try_recv() {
                                    Ok(Some(m)) if m.envelope.session_id == session_id => {
                                        if let Some(ev) = m.event {
                                            pump_process(
                                                &ev, &mut last_error, &sink, &session_id,
                                                effective_context_window,
                                            ).await;
                                        }
                                    }
                                    Ok(Some(_)) => {}
                                    Ok(None) => break,
                                    Err(SubscriptionError::Lagged(n)) => {
                                        tracing::warn!(n, "event subscription lagged, events dropped");
                                        break;
                                    }
                                    Err(SubscriptionError::Closed) => break,
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }

        // End Langfuse trace and flush（构造 JoinHandle；drop = detach，不阻塞管线）
        let langfuse_flush = langfuse_on_turn_end.as_ref().and_then(|f| f(last_error));

        // Resolve stop_reason from the oneshot channel set by executor
        let stop_reason = stop_reason_rx.await.unwrap_or(PromptStopReason::EndTurn);
        let stop_reason_str = match stop_reason {
            PromptStopReason::EndTurn => "end_turn",
            PromptStopReason::Cancelled => "cancelled",
            PromptStopReason::MaxTurnRequests => "max_turn_requests",
        };
        sink.push_done(&session_id, stop_reason_str, request_id.as_deref())
            .await;

        // Signal pump completion BEFORE Langfuse flush.
        // Langfuse is telemetry — it must never block the execution pipeline.
        // Without this, a slow/unreachable Langfuse API blocks pump_done_tx,
        // which blocks wait_for_pump(), which blocks run_session_loop() from
        // returning, which holds the prompt_lock and prevents the next prompt
        // from starting. Ctrl+C can't recover because the new prompt's cancel
        // is a fresh token.
        let _ = pump_done_tx.send(());

        // Langfuse flush: fire-and-forget. The spawned task runs independently;
        // worst-case it blocks for ~150s (HTTP 30s × 3 retries + backoff) then
        // logs warnings. The pump has already signaled completion above, so this
        // never blocks the execution pipeline.
        drop(langfuse_flush);
    });

    PumpHandle { pump_done_rx }
}
