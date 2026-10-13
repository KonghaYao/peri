//! 观测 input/output 的冻结、来源适配与事件所有权（契约 §3.3）。
//!
//! [`SnapshotSource`] 可由 `Arc<Value>` 或 controller 的 messages/tools 借用序列化适配器实现，
//! 但适配器不依赖文件系统；`Batcher::try_freeze` 同步计费并登记准备任务，IO 随 worker 执行。
//!
//! 类型均 `Send + Sync`；[`MemoryPermit`]、冻结字节与 [`SnapshotTicket`] 均不可 Clone。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::error::LangfuseError;
use crate::memory::{MemoryBudget, MemoryPermit};
use crate::payload::InputSnapshot;
use crate::stable_json::StableJson;
use crate::types::IngestionEvent;

/// 可被遥测借用并按紧凑 JSON 冻结的来源。
///
/// 实现必须给出包含其树容量的保守上界；未知动态分配量应返回错误而不是记 0。
/// 适配器只克隆已存在的 `Arc`，不构造整棵树。
pub trait SnapshotSource: Send + Sync + 'static {
    /// 保守的 retained 上界；必须是无大分配遍历。
    fn retained_bytes(&self) -> Result<usize, LangfuseError>;

    /// 写出紧凑 JSON 字节（不是 OTLP 转义后的字符串）；允许被多次调用。
    fn write_json(&self, out: &mut dyn std::io::Write) -> Result<(), LangfuseError>;
}

/// 以冻结正文的稳定键序写出 `Value`（对象键字典序、数组保序）。
///
/// 冻结字节必须与编译期 `serde_json/preserve_order` 是否启用无关（契约 §3.4）：本 crate 的
/// [`ValueSource`] 与 controller 的 messages/tools 借用适配器都经这里输出，键序只有一处口径。
pub fn write_stable_json<W: io::Write + ?Sized>(
    out: &mut W,
    value: &serde_json::Value,
) -> Result<(), LangfuseError> {
    serde_json::to_writer(out, &StableJson(value))?;
    Ok(())
}

/// 冻结准入票据；ticket Drop 只置取消位并唤醒 worker，不 spawn、不积累清理消息。
pub struct SnapshotTicket {
    pub(crate) cell: Arc<SnapshotCell>,
}

/// 准备任务的共享单元；与各 permit 一样不可伪造地保存 [`MemoryBudget`] 身份。
pub(crate) struct SnapshotCell {
    pub(crate) budget: Arc<MemoryBudget>,
    pub(crate) prepare_seq: u64,
    pub(crate) state: Mutex<SnapshotState>,
    pub(crate) cancelled: AtomicBool,
    pub(crate) changed: Notify,
}

/// 准备任务的可见状态；`Failed` 保留分类错误供屏障返回，不携带正文。
pub(crate) enum SnapshotState {
    Preparing,
    Ready(InputSnapshot),
    Failed(LangfuseError),
    Taken,
}

/// 事件中 input/output 的来源：已就绪，或等待准备收敛的票据。
pub enum InputSource {
    Ready(InputSnapshot),
    Preparing(SnapshotTicket),
}

/// 进入 Batcher 准入的遥测事件；持有事件原文 permit 与 input/output 来源。
pub struct TelemetryEvent {
    pub(crate) event: IngestionEvent,
    pub(crate) input: Option<InputSource>,
    pub(crate) output: Option<InputSource>,
    pub(crate) permit: MemoryPermit,
}

impl TelemetryEvent {
    /// 同步构造入口（替代 `Batcher::try_add` 的直接事件路径）。
    ///
    /// 校验 event permit、input/output 的 Inline permit、Spooled 清理注册及 Preparing cell 的
    /// 预算身份全部 `Arc::ptr_eq` 指向 `budget`，否则返回 [`LangfuseError::BudgetMismatch`]；
    /// 有替代来源时对应 body 字段必须为 `None`，否则返回 Config 错误防止双份内容。
    ///
    /// 事件原文的 retained 额度在本入口按有界计量（不构造整棵 OTLP 树）预约：冻结契约的
    /// `try_new` 签名不携带 `TelemetryConfig`，因此 payload 输出 allowance（spool 阈值前缀 /
    /// 纯内存 `max_wire_bytes` 上界，契约 §4）由知道配置的 Batcher 准入（P3a）另行预约。
    /// 任何失败路径都按原 owner 消费并 drop 来源：Inline 归还 permit、Spooled 交原清理槽、
    /// Preparing 票据置取消位。
    pub fn try_new(
        event: IngestionEvent,
        input: Option<InputSource>,
        output: Option<InputSource>,
        budget: &Arc<MemoryBudget>,
    ) -> Result<Self, LangfuseError> {
        let (body_input, body_output) = body_slots(&event);
        if (input.is_some() && body_input) || (output.is_some() && body_output) {
            return Err(LangfuseError::Config(
                "telemetry event carries both an inline body field and a frozen snapshot source"
                    .into(),
            ));
        }
        for source in [input.as_ref(), output.as_ref()].into_iter().flatten() {
            if !source_belongs_to(source, budget) {
                return Err(LangfuseError::BudgetMismatch);
            }
        }
        let permit = budget.try_reserve(measure_event(&event)?)?;
        Ok(Self {
            event,
            input,
            output,
            permit,
        })
    }
}

impl Drop for SnapshotTicket {
    fn drop(&mut self) {
        self.cell.cancelled.store(true, Ordering::Release);
        // 只唤醒已注册的等待者：不 spawn、不入队、不阻塞 Drop。
        self.cell.changed.notify_waiters();
    }
}

/// `InputSource` 是否绑定到同一份部署预算（仅金额相同不算）。
fn source_belongs_to(source: &InputSource, budget: &Arc<MemoryBudget>) -> bool {
    match source {
        InputSource::Ready(InputSnapshot::Inline(bytes)) => {
            Arc::ptr_eq(bytes.permit().budget(), budget)
        }
        InputSource::Ready(InputSnapshot::Spooled(object)) => {
            Arc::ptr_eq(object.cleanup.slot.budget(), budget)
        }
        InputSource::Preparing(ticket) => Arc::ptr_eq(&ticket.cell.budget, budget),
    }
}

/// 事件 body 是否自带 input/output 槽位（有替代来源时它们必须为 `None`）。
fn body_slots(event: &IngestionEvent) -> (bool, bool) {
    let slots = |input: &Option<serde_json::Value>, output: &Option<serde_json::Value>| {
        (input.is_some(), output.is_some())
    };
    match event {
        IngestionEvent::TraceCreate { body, .. } => slots(&body.input, &body.output),
        IngestionEvent::SpanCreate { body, .. } | IngestionEvent::SpanUpdate { body, .. } => {
            slots(&body.input, &body.output)
        }
        IngestionEvent::GenerationCreate { body, .. }
        | IngestionEvent::GenerationUpdate { body, .. } => slots(&body.input, &body.output),
        IngestionEvent::EventCreate { body, .. } => slots(&body.input, &body.output),
        IngestionEvent::ObservationCreate { body, .. }
        | IngestionEvent::ObservationUpdate { body, .. } => slots(&body.input, &body.output),
        IngestionEvent::ScoreCreate { .. }
        | IngestionEvent::SdkLog { .. }
        | IngestionEvent::SessionCreate { .. }
        | IngestionEvent::SessionUpdate { .. } => (false, false),
    }
}

/// 事件原文的 serde-JSON 字节数；计数 sink 不保留任何字节。
///
/// 这是事件保留额的**唯一**计量来源：[`TelemetryEvent::try_new`] 按它挂账，direct ingest 也
/// 按它推导本地预算上界，两处不得各自估算。
pub(crate) fn measure_event(event: &IngestionEvent) -> Result<usize, LangfuseError> {
    let mut sink = CountSink(0);
    serde_json::to_writer(&mut sink, event)?;
    Ok(sink.0)
}

/// 只累加长度的 `Write` 实现；写入永不失败。
struct CountSink(usize);

impl io::Write for CountSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 借用 `Arc<serde_json::Value>` 的零拷贝 source 适配器。
///
/// 调用方在跨事件保留该 `Arc` 之前必须先经 `Batcher::try_freeze` 完成冻结准入。
pub fn snapshot_value(value: &Arc<serde_json::Value>) -> Arc<dyn SnapshotSource> {
    Arc::new(ValueSource {
        value: Arc::clone(value),
    })
}

/// `Arc<Value>` 的借用视图：只克隆 Arc，不重建 JSON 树。
struct ValueSource {
    value: Arc<serde_json::Value>,
}

impl SnapshotSource for ValueSource {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        // Arc 头 + 根节点自身，再逐层累加容器容量；Value 树的每个节点只计一次。
        let mut total = std::mem::size_of::<serde_json::Value>() + 2 * std::mem::size_of::<usize>();
        account_tree(&self.value, &mut total)?;
        Ok(total)
    }

    fn write_json(&self, out: &mut dyn io::Write) -> Result<(), LangfuseError> {
        write_stable_json(out, &self.value)
    }
}

/// 递归累加 JSON 树的保守 heap 容量；溢出按配置错误上报而不是回绕。
fn account_tree(value: &serde_json::Value, total: &mut usize) -> Result<(), LangfuseError> {
    let overflow = || LangfuseError::Config("snapshot retained accounting overflows".into());
    let add = |total: &mut usize, bytes: usize| -> Result<(), LangfuseError> {
        *total = total.checked_add(bytes).ok_or_else(overflow)?;
        Ok(())
    };
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(())
        }
        serde_json::Value::String(text) => add(total, text.capacity()),
        serde_json::Value::Array(items) => {
            add(
                total,
                items.len() * std::mem::size_of::<serde_json::Value>(),
            )?;
            for item in items {
                add(total, std::mem::size_of::<serde_json::Value>())?;
                account_tree(item, total)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            add(
                total,
                map.len()
                    * (std::mem::size_of::<String>() + std::mem::size_of::<serde_json::Value>()),
            )?;
            for (key, item) in map {
                add(
                    total,
                    key.capacity() + std::mem::size_of::<serde_json::Value>(),
                )?;
                account_tree(item, total)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "snapshot_test.rs"]
mod tests;
