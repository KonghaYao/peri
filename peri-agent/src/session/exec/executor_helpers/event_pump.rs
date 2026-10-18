use std::sync::Arc;

use peri_acp_types::{
    command::PromptStopReason,
    event::{EventSink, EventSinkError, EventSubscriber, ExecutorEvent, SubscriptionError},
    identity::SessionSeq,
    session::TurnTelemetryOutcome,
};
use tokio::sync::oneshot;
use tracing::debug;

use crate::session::user_input_mailbox::UserInputMailbox;

/// Langfuse trace 收尾闭包（构造 JoinHandle 后由 pump 在 pump_done 之后 drop）。
pub type LangfuseEndFn =
    Arc<dyn Fn(TurnTelemetryOutcome) -> Option<tokio::task::JoinHandle<()>> + Send + Sync>;

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
    pub stop_reason_rx: oneshot::Receiver<(PromptStopReason, TurnTelemetryOutcome)>,
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
    /// 输入交付日志：事件泵在推送任何后续事件前，按实际交付顺序补发
    /// `UserInputDelivered`。render/state 事件的丢失不再决定气泡是否存在。
    pub delivery_journal: Option<Arc<UserInputMailbox>>,
}

/// 事件泵已推送的交付身份（有界去重，覆盖迟到重复事件）。
#[derive(Default)]
struct DeliveryEmissions {
    seen: std::collections::HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl DeliveryEmissions {
    const CAPACITY: usize = 512;

    fn insert(&mut self, input_id: &str) -> bool {
        if !self.seen.insert(input_id.to_owned()) {
            return false;
        }
        self.order.push_back(input_id.to_owned());
        while self.order.len() > Self::CAPACITY {
            if let Some(expired) = self.order.pop_front() {
                self.seen.remove(&expired);
            }
        }
        true
    }

    fn contains(&self, input_id: &str) -> bool {
        self.seen.contains(input_id)
    }
}

/// 交付日志的推送水位。
///
/// `session_seq` 是事件与交付顺序的唯一权威：只允许在序号严格更小的当前事件
/// 之前补发已绑定序号的交付。Receive 在进入有损 render 通道前完成预留与绑定，
/// 未绑定条目只来自无预留能力的降级路径，只能在尾部补发。
#[derive(Clone, Copy)]
enum DeliveryCutoff {
    /// 订阅路径：只发 `session_seq < 当前事件序号` 的交付。
    Before(SessionSeq),
    /// `event_rx` 防御分支（无 envelope，生产无发送点）：保持旧行为。
    BeforeUnsequenced,
    /// 尾部：发全部（含未绑定）。
    Tail,
}

impl DeliveryCutoff {
    fn allows(self, session_seq: Option<SessionSeq>) -> bool {
        match self {
            Self::Before(current) => session_seq.is_some_and(|seq| seq < current),
            Self::BeforeUnsequenced | Self::Tail => true,
        }
    }
}

/// 事件泵推送 sink 所需的固定上下文（避免逐层传递同组参数）。
struct PumpSinkContext<'a> {
    sink: &'a Arc<dyn EventSink>,
    session_id: &'a str,
    effective_context_window: u32,
    delivery_journal: Option<&'a UserInputMailbox>,
}

/// 在推送当前事件之前排空交付日志。锁只在复制快照时持有，等待 sink 期间不持锁。
///
/// 返回 `Err` 时保留未确认项（已成功推送的项才 ack），由下一次 drain 重试。
/// 日志快照的锁获取就是顺序线性化点：此前完成的 `mark_delivered` 必在快照中，
/// 此后的交付按当前事件之后排序。是否补发由 [`DeliveryCutoff`] 按事件序号决定。
async fn drain_delivery_journal(
    ctx: &PumpSinkContext<'_>,
    emitted: &mut DeliveryEmissions,
    cutoff: DeliveryCutoff,
) -> Result<(), EventSinkError> {
    let Some(mailbox) = ctx.delivery_journal else {
        return Ok(());
    };
    for pending in mailbox.pending_delivery_notifications() {
        if emitted.contains(&pending.input_id) {
            mailbox.ack_delivery_notification(&pending.input_id);
            continue;
        }
        if !cutoff.allows(pending.session_seq) {
            continue;
        }
        ctx.sink
            .push_event(
                ctx.session_id,
                &ExecutorEvent::UserInputDelivered {
                    generation: mailbox.generation().to_owned(),
                    input_id: pending.input_id.clone(),
                    content: pending.content,
                },
                ctx.effective_context_window,
            )
            .await?;
        // 只有 transport 确认写出后才进入去重集合并 ack；失败时保留下次重试。
        let _ = emitted.insert(&pending.input_id);
        mailbox.ack_delivery_notification(&pending.input_id);
    }
    Ok(())
}

/// 后台事件泵句柄，通过 oneshot channel 与 pump_done_rx 配对。
pub struct PumpHandle {
    pub pump_done_rx: oneshot::Receiver<()>,
}

/// 排空交付日志。失败时保留未确认项，由调用方重试或显式失败。
async fn flush_deliveries(
    ctx: &PumpSinkContext<'_>,
    emitted_deliveries: &mut DeliveryEmissions,
    cutoff: DeliveryCutoff,
) -> Result<(), EventSinkError> {
    drain_delivery_journal(ctx, emitted_deliveries, cutoff).await
}

/// 单条事件的处理（Langfuse error_kind 捕获 / bg callback unstable 事件 /
/// 协议化推送）。事件循环与 drain 分支共用，保证两条路径处理语义一致。
async fn pump_process(
    exec_event: &ExecutorEvent,
    last_error: &mut Option<String>,
    ctx: &PumpSinkContext<'_>,
    emitted_deliveries: &mut DeliveryEmissions,
    cutoff: DeliveryCutoff,
) -> Result<(), EventSinkError> {
    // 交付确认必须先于任何后续 assistant 输出进入 sink。Receive 在把交付事件
    // 放入有损 render 通道前已预留 `session_seq` 并绑定日志条目，因此即使 wire
    // 事件被丢弃，也能在下一个更大序号的事件之前按序补发；只有无预留能力的
    // 降级路径（未注册 session）才会留到尾部。
    flush_deliveries(ctx, emitted_deliveries, cutoff).await?;
    // 队列快照由会话订阅投递，运行开始由宿主在执行前可靠发送；本轮泵跳过两者。
    if matches!(
        exec_event,
        ExecutorEvent::UserInputQueueChanged(_) | ExecutorEvent::UserInputRunStarted { .. }
    ) {
        return Ok(());
    }
    // 交付日志已经按序推送过该身份；迟到/重复的 wire 事件不得再次入渲染 FIFO。
    if ctx.delivery_journal.is_some() {
        if let ExecutorEvent::UserInputDelivered { input_id, .. } = exec_event {
            if emitted_deliveries.contains(input_id) {
                return Ok(());
            }
        }
    }
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
        flush_deliveries(ctx, emitted_deliveries, cutoff).await?;
        if let ExecutorEvent::MessageAdded(msg) = exec_event {
            let text = msg.content();
            ctx.sink
                .push_unstable_event(
                    ctx.session_id,
                    "bg-callback-user-message".to_string(),
                    serde_json::json!({ "text": text }),
                )
                .await;
        }
    }

    // 线性化点：在真正送入 sink 之前再次取日志快照。此前（同一 mailbox 锁内）
    // 完成的 mark_delivered 一定在快照中并先发；此后完成的交付排在当前事件之后。
    // 只有序号严格小于当前事件的交付才允许越过当前事件，未绑定序号的留到尾部。
    flush_deliveries(ctx, emitted_deliveries, cutoff).await?;
    if ctx.delivery_journal.is_some() {
        if let ExecutorEvent::UserInputDelivered { input_id, .. } = exec_event {
            if emitted_deliveries.contains(input_id) {
                return Ok(());
            }
        }
    }

    ctx.sink
        .push_event(ctx.session_id, exec_event, ctx.effective_context_window)
        .await?;
    // 当前事件本身就是交付确认：写入 sink 成功后立即去重并 ack，避免
    // journal 条目因“序号相等不提前补发”而在下一次事件重复推送。
    if let Some(mailbox) = ctx.delivery_journal {
        if let ExecutorEvent::UserInputDelivered { input_id, .. } = exec_event {
            emitted_deliveries.insert(input_id);
            mailbox.ack_delivery_notification(input_id);
        }
    }
    Ok(())
}

const EVENT_SINK_RETRY_ATTEMPTS: usize = 3;

/// 有界重试当前事件；只有 sink 明确拒绝时才重试，耗尽后由调用方显式失败。
async fn pump_process_with_retries(
    exec_event: &ExecutorEvent,
    last_error: &mut Option<String>,
    ctx: &PumpSinkContext<'_>,
    emitted_deliveries: &mut DeliveryEmissions,
    cutoff: DeliveryCutoff,
) -> Result<(), EventSinkError> {
    let mut last_failure = None;
    for attempt in 0..EVENT_SINK_RETRY_ATTEMPTS {
        match pump_process(exec_event, last_error, ctx, emitted_deliveries, cutoff).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    session_id = %ctx.session_id,
                    attempt,
                    error = %error,
                    "event sink push failed; retrying"
                );
                last_failure = Some(error);
                if attempt + 1 < EVENT_SINK_RETRY_ATTEMPTS {
                    peri_time::sleep(std::time::Duration::from_millis(10 * (attempt as u64 + 1)))
                        .await;
                }
            }
        }
    }
    Err(last_failure.expect("at least one retry attempt"))
}

/// 重试耗尽后显式通知客户端；失败通知本身也使用同一有界重试语义。
/// 若它仍不可达，客户端只能依赖 transport 关闭路径收敛，泵仍会发 pump_done。
async fn emit_delivery_failure(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    error: &EventSinkError,
) -> Result<(), EventSinkError> {
    let event = ExecutorEvent::AgentExecutionFailed {
        message: format!("event delivery failed after retries: {error}"),
    };
    let mut last_failure = None;
    for attempt in 0..EVENT_SINK_RETRY_ATTEMPTS {
        match sink.push_event(session_id, &event, 0).await {
            Ok(()) => return Ok(()),
            Err(push_error) => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt,
                    error = %push_error,
                    "AgentExecutionFailed delivery failed; retrying"
                );
                last_failure = Some(push_error);
                if attempt + 1 < EVENT_SINK_RETRY_ATTEMPTS {
                    peri_time::sleep(std::time::Duration::from_millis(10 * (attempt as u64 + 1)))
                        .await;
                }
            }
        }
    }
    let last_failure = last_failure.expect("at least one retry attempt");
    tracing::error!(
        session_id = %session_id,
        error = %last_failure,
        "AgentExecutionFailed unreachable after retries; client must converge via transport close"
    );
    Err(last_failure)
}

/// 尾部交付重试；耗尽后返回错误，调用方不得再发正常 done。
async fn flush_trailing_with_retries(
    ctx: &PumpSinkContext<'_>,
    emitted_deliveries: &mut DeliveryEmissions,
) -> Result<(), EventSinkError> {
    let mut last_failure = None;
    for attempt in 0..EVENT_SINK_RETRY_ATTEMPTS {
        match flush_deliveries(ctx, emitted_deliveries, DeliveryCutoff::Tail).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    session_id = %ctx.session_id,
                    attempt,
                    error = %error,
                    "trailing delivery push failed; retrying"
                );
                last_failure = Some(error);
                if attempt + 1 < EVENT_SINK_RETRY_ATTEMPTS {
                    peri_time::sleep(std::time::Duration::from_millis(10 * (attempt as u64 + 1)))
                        .await;
                }
            }
        }
    }
    Err(last_failure.expect("at least one retry attempt"))
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
        delivery_journal,
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
        let mut emitted_deliveries = DeliveryEmissions::default();
        let mut delivery_failed = false;
        let ctx = PumpSinkContext {
            sink: &sink,
            session_id: &session_id,
            effective_context_window,
            delivery_journal: delivery_journal.as_deref(),
        };

        loop {
            tokio::select! {
                biased;
                msg = subscription.recv() => {
                    match msg {
                        Ok(m) => {
                            // 订阅是全局广播：只处理本 session 的事件
                            if m.envelope.session_id == session_id {
                                let session_seq = m.envelope.session_seq;
                                if let Some(ev) = m.event {
                                    if let Err(error) = pump_process_with_retries(
                                        &ev, &mut last_error, &ctx,
                                        &mut emitted_deliveries,
                                        DeliveryCutoff::Before(session_seq),
                                    ).await {
                                        tracing::error!(session_id = %session_id, error = %error, "event delivery retries exhausted");
                                        let _ = emit_delivery_failure(&sink, &session_id, &error).await;
                                        delivery_failed = true;
                                        break;
                                    }
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
                            if let Err(error) = pump_process_with_retries(
                                &exec_event, &mut last_error, &ctx,
                                &mut emitted_deliveries,
                                DeliveryCutoff::BeforeUnsequenced,
                            ).await {
                                tracing::error!(session_id = %session_id, error = %error, "event delivery retries exhausted");
                                let _ = emit_delivery_failure(&sink, &session_id, &error).await;
                                delivery_failed = true;
                                break;
                            }
                        }
                        None => {
                            // 发射点集合全部结束（event_tx 全 drop）：drain 广播中
                            // 已入 buffer 的在途事件（broadcast send 同步入 buffer，
                            // 关闭后到达的事件与 pump 退出后的丢弃语义一致——
                            // 对应迁移前 close_channel 后 forwarder 检查 None 丢弃）。
                            loop {
                                match subscription.try_recv() {
                                    Ok(Some(m)) if m.envelope.session_id == session_id => {
                                        let session_seq = m.envelope.session_seq;
                                        if let Some(ev) = m.event {
                                            if let Err(error) = pump_process_with_retries(
                                                &ev, &mut last_error, &ctx,
                                                &mut emitted_deliveries,
                                                DeliveryCutoff::Before(session_seq),
                                            ).await {
                                                tracing::error!(session_id = %session_id, error = %error, "event delivery retries exhausted");
                                                let _ = emit_delivery_failure(&sink, &session_id, &error).await;
                                                delivery_failed = true;
                                                break;
                                            }
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

        // 尾部交付：turn 结束后若仍有未推送确认（例如最后一个 render 事件丢失），
        // 在 done 之前补发；重试耗尽则显式失败，不再以正常 done 收尾。
        if !delivery_failed {
            if let Err(error) = flush_trailing_with_retries(&ctx, &mut emitted_deliveries).await {
                tracing::error!(
                    session_id = %session_id,
                    error = %error,
                    "trailing user input delivery failed after retries"
                );
                let _ = emit_delivery_failure(&sink, &session_id, &error).await;
                delivery_failed = true;
            }
        }

        // Executor result is the canonical terminal source; broadcast events remain observational.
        let (stop_reason, telemetry_outcome) = stop_reason_rx.await.unwrap_or((
            PromptStopReason::EndTurn,
            TurnTelemetryOutcome::Failed {
                failure: Box::new(peri_acp_types::session::ExecutionFailure::internal(
                    "Agent execution result was unavailable",
                )),
            },
        ));

        // End Langfuse trace and flush（构造 JoinHandle；drop = detach，不阻塞管线）
        let langfuse_flush = langfuse_on_turn_end
            .as_ref()
            .and_then(|f| f(telemetry_outcome));

        let stop_reason_str = super::done_stop_reason(stop_reason);
        if !delivery_failed {
            sink.push_done(&session_id, stop_reason_str, request_id.as_deref())
                .await;
        } else {
            tracing::error!(
                session_id = %session_id,
                "skip normal done after delivery retry exhaustion; AgentExecutionFailed already emitted"
            );
        }

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
