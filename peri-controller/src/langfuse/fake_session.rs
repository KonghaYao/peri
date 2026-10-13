use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use langfuse_client::{
    FlushReport, IngestionEvent, InputSource, LangfuseError, MemoryBudget, SnapshotSource,
    TelemetryEvent,
};
use parking_lot::Mutex;

use super::drop_telemetry::LangfuseDropRegistry;
use super::session_like::LangfuseSessionLike;
use super::turn_traces::TurnTraceRegistry;

/// 假会话预算额度：与契约 §6 的 `memory_budget_bytes` 默认值同额。
///
/// 假会话不是部署，但 `TelemetryEvent::try_new` 的预算身份校验必须与真实部署一样逐项生效，
/// 因此这里持有真实的 [`MemoryBudget`]（不可伪造、不可替换），而不是绕过预算。
const FAKE_MEMORY_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// 假会话没有部署能力（无 worker、无暂存、无预算预留），只记录**直接事件**。
///
/// 冻结来源与已冻结事件由真实 `Batcher` 产生（`InputSource`/`TelemetryEvent` 均不可在
/// crate 外伪造，契约 §3.3），因此本实现如实失败，不返回假 `Ok`、也不伪造来源。
/// 需要冻结正文/OTLP 逐字节断言的用例使用真实 Batcher fixture（见 `tracer/input_snapshot_test.rs`）。
pub struct FakeLangfuseSession {
    events: Mutex<Vec<IngestionEvent>>,
    budget: Arc<MemoryBudget>,
    drop_registry: LangfuseDropRegistry,
    turn_traces: TurnTraceRegistry,
    session_id: String,
    flush_count: AtomicUsize,
}

impl FakeLangfuseSession {
    pub fn new(session_id: impl Into<String>) -> Arc<Self> {
        let budget = MemoryBudget::new(FAKE_MEMORY_BUDGET_BYTES)
            .expect("固定的非零假会话预算必须被 MemoryBudget 接受");
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            budget,
            drop_registry: LangfuseDropRegistry::default(),
            turn_traces: TurnTraceRegistry::default(),
            session_id: session_id.into(),
            flush_count: AtomicUsize::new(0),
        })
    }

    pub fn events_snapshot(&self) -> Vec<IngestionEvent> {
        self.events.lock().clone()
    }

    pub fn event_count(&self) -> usize {
        self.events.lock().len()
    }

    pub fn flush_count(&self) -> usize {
        self.flush_count.load(Ordering::SeqCst)
    }
}

impl LangfuseSessionLike for FakeLangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.events.lock().push(event);
        Ok(())
    }

    /// 假会话没有冻结能力：`InputSource` 只能由真实 Batcher 的 `try_freeze` 产生（预算身份不可
    /// 伪造），这里必须如实失败，让 tracer 走 `GenerationInput::Rejected` 而不是伪造一个来源。
    fn try_freeze(&self, _source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError> {
        Err(LangfuseError::Config(
            "fake session cannot freeze input snapshots".into(),
        ))
    }

    /// 已冻结事件在假会话里没有可交付路径，也没有可读取的正文（契约 §3.3 不透明类型）；
    /// 如实拒绝，不用 `Ok` 冒充入队。
    fn try_add_prepared(&self, _event: TelemetryEvent) -> Result<(), LangfuseError> {
        Err(LangfuseError::Config(
            "fake session cannot accept prepared telemetry events".into(),
        ))
    }

    fn memory_budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }

    /// 假会话把已接受的事件同步记在进程内，没有待发送积压：一次 flush 即覆盖全部记录，
    /// 因此报告 `Complete`（不冒充真实部署的交付语义）。
    fn flush(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FlushReport, LangfuseError>> + Send + '_>> {
        self.flush_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(FlushReport::Complete) })
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drop_registry
    }

    fn turn_traces(&self) -> &TurnTraceRegistry {
        &self.turn_traces
    }
}
