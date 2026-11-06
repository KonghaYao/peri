//! 结果收集的终态收敛：事件泵未完成时不得按成功结算。
//!
//! 回归 issue #206：事件泵被阻塞 sink 卡住超过 10 秒时不能按成功结算，
//! 否则客户端拿不到 `done` 只能一直 loading。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::{
    command::PromptStopReason,
    event::{
        EventMessage, EventSink, EventSinkError, EventSubscriber, ExecutorEvent, SubscriptionError,
    },
    messages::MessageId,
    session::{ExecutionFailure, TurnTelemetryOutcome},
};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::agent::state::AgentState;

use super::{
    collect_result, spawn_event_pump, CollectRequest, ExecOutcome, PumpHandle, SpawnPumpRequest,
};

type EventTx = Arc<parking_lot::Mutex<Option<mpsc::UnboundedSender<ExecutorEvent>>>>;

/// 永不产生广播事件的订阅者，让泵只消费 `event_rx` 与 sink。
struct PendingSubscriber;

#[async_trait]
impl EventSubscriber for PendingSubscriber {
    /// 保持订阅挂起，模拟无外部事件源。
    async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        std::future::pending().await
    }

    /// 订阅无积压事件。
    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        Ok(None)
    }
}

/// 在首个 assistant 事件处阻塞 sink，模拟慢客户端 / transport 背压。
struct BlockingSink {
    entered: Notify,
    release: Notify,
    done: Notify,
    done_count: AtomicUsize,
}

impl BlockingSink {
    /// 构造未阻塞、未送达 done 的 sink。
    fn new() -> Self {
        Self {
            entered: Notify::new(),
            release: Notify::new(),
            done: Notify::new(),
            done_count: AtomicUsize::new(0),
        }
    }

    /// 已送达的 done 次数。
    fn done_count(&self) -> usize {
        self.done_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EventSink for BlockingSink {
    /// 首个 assistant 事件通知测试并等待放行。
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        if matches!(event, ExecutorEvent::TextChunk { .. }) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }

    /// 记录 done 并唤醒等待者。
    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.done_count.fetch_add(1, Ordering::SeqCst);
        self.done.notify_one();
    }
}

/// 启动泵并预置成功终态，返回事件发送端与泵句柄。
fn start_pump(sink: Arc<dyn EventSink>) -> (EventTx, PumpHandle) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = oneshot::channel();
    let handle = spawn_event_pump(SpawnPumpRequest {
        subscription: Box::new(PendingSubscriber),
        event_rx,
        stop_reason_rx: stop_rx,
        sink,
        session_id: "test-session".into(),
        effective_context_window: 0,
        langfuse_on_turn_start: None,
        langfuse_on_turn_end: None,
        request_id: None,
        delivery_journal: None,
    });
    stop_tx
        .send((PromptStopReason::EndTurn, TurnTelemetryOutcome::Completed))
        .expect("stop reason receiver must be alive");
    (Arc::new(parking_lot::Mutex::new(Some(event_tx))), handle)
}

/// 构造无失败、`ok` 指定的执行结果。
fn exec_outcome(ok: bool) -> ExecOutcome {
    ExecOutcome {
        ok,
        stop_reason: PromptStopReason::EndTurn,
        failure: None,
        history_replaced_by_compaction: false,
        persisted_payloads: Vec::new(),
        persistence_inconsistent: false,
        agent_state: AgentState::new("/tmp"),
    }
}

/// 构造一条 assistant 文本事件。
fn text_chunk() -> ExecutorEvent {
    ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: "answer".into(),
        source_agent_id: None,
    }
}

/// 启动一个已阻塞在首个 assistant 事件上的泵。
async fn start_stalled_pump() -> (Arc<BlockingSink>, EventTx, PumpHandle) {
    let sink = Arc::new(BlockingSink::new());
    let (event_tx, pump) = start_pump(sink.clone());
    event_tx
        .lock()
        .as_ref()
        .expect("event sender open")
        .send(text_chunk())
        .expect("pump receiver open");
    // 确认泵已经阻塞在 sink 内，而不是尚未开始处理事件。
    sink.entered.notified().await;
    (sink, event_tx, pump)
}

/// 正常泵：事件排空、done 送达后仍按执行结果成功结算。
#[tokio::test]
async fn completed_pump_keeps_success_result() {
    let sink = Arc::new(BlockingSink::new());
    let (event_tx, pump) = start_pump(sink.clone());
    event_tx
        .lock()
        .as_ref()
        .expect("event sender open")
        .send(text_chunk())
        .expect("pump receiver open");
    sink.entered.notified().await;
    sink.release.notify_one();

    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle: pump,
        session_id: "test-session",
        exec_outcome: exec_outcome(true),
        pending_tasks: 0,
    })
    .await;

    assert!(result.ok, "正常完成的泵不得改变成功终态");
    assert!(result.failure.is_none());
    assert_eq!(sink.done_count(), 1, "正常路径必须送达 done");
}

/// 泵任务异常退出、完成信号未发出：同样不能按成功结算。
#[tokio::test]
async fn pump_signal_channel_closed_without_done_cannot_return_success() {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let event_tx = Arc::new(parking_lot::Mutex::new(Some(event_tx)));
    let (done_tx, done_rx) = oneshot::channel();
    drop(done_tx);

    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle: PumpHandle {
            pump_done_rx: done_rx,
        },
        session_id: "test-session",
        exec_outcome: exec_outcome(true),
        pending_tasks: 0,
    })
    .await;

    assert!(!result.ok, "泵提前结束不得按成功结算");
    let failure = result.failure.expect("泵提前结束必须携带 fatal failure");
    assert!(
        failure.public_message.contains("event pump"),
        "failure 必须说明事件泵未完成：{}",
        failure.public_message
    );
}

/// 泵被阻塞 sink 卡住超过 10 秒：prompt 必须结算为失败，不能按成功返回。
#[tokio::test(start_paused = true)]
async fn stalled_pump_beyond_ten_seconds_cannot_return_success() {
    let (sink, event_tx, pump) = start_stalled_pump().await;

    let started = tokio::time::Instant::now();
    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle: pump,
        session_id: "test-session",
        exec_outcome: exec_outcome(true),
        pending_tasks: 0,
    })
    .await;
    let elapsed = started.elapsed();

    assert!(!result.ok, "事件泵超时不得按成功结算");
    let failure = result
        .failure
        .expect("事件泵超时必须携带 fatal failure，才能让 ACP 返回错误");
    assert!(
        failure.public_message.contains("event pump"),
        "failure 必须说明事件泵未完成：{}",
        failure.public_message
    );
    assert!(
        elapsed >= Duration::from_secs(10),
        "必须等待完整的 10 秒窗口：{elapsed:?}"
    );
    assert_eq!(
        sink.done_count(),
        0,
        "超时返回时 done 尚未送达，这正是不能报告成功的原因"
    );

    // 释放泵让任务收敛，验证阻塞解除后 done 仍会被投递（迟到但不丢失）。
    sink.release.notify_one();
    sink.done.notified().await;
    assert_eq!(sink.done_count(), 1);
}

/// 已有执行失败时保留原归因，同时把终态未交付记录为附加 cause。
#[tokio::test(start_paused = true)]
async fn stalled_pump_preserves_existing_failure_and_records_delivery_gap() {
    let (sink, event_tx, pump) = start_stalled_pump().await;
    let mut outcome = exec_outcome(false);
    outcome.failure = Some(ExecutionFailure::internal("original execution failure"));

    let result = collect_result(CollectRequest {
        event_tx: &event_tx,
        pump_handle: pump,
        session_id: "test-session",
        exec_outcome: outcome,
        pending_tasks: 0,
    })
    .await;

    assert!(!result.ok);
    let failure = result.failure.expect("已有失败必须保留");
    assert_eq!(
        failure.public_message, "original execution failure",
        "原始执行失败归因不得被泵超时覆盖"
    );
    assert!(
        failure
            .causes
            .iter()
            .any(|cause| cause.contains("event pump")),
        "终态未交付必须作为附加 cause 保留：{:?}",
        failure.causes
    );

    sink.release.notify_one();
    sink.done.notified().await;
}
