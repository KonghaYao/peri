use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use langfuse_client::{
    FlushReport, IngestionEvent, InputSource, LangfuseError, MemoryBudget, SnapshotSource,
    TelemetryEvent,
};

use super::drop_telemetry::LangfuseDropRegistry;
use super::turn_traces::TurnTraceRegistry;

/// Langfuse session 抽象，让 tracer 可注入 fake session 跑单测。
///
/// 冻结契约为契约 §3.6：`try_freeze`/`try_add_prepared`/`memory_budget` 与返回交付报告的
/// `flush` 是本 trait 的必备方法，实现体不得用默认 `Ok`/`None` 冒充落地。
pub trait LangfuseSessionLike: Send + Sync {
    /// 同步添加**直接事件**（无冻结来源）到批量队列
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError>;
    /// 冻结准入：同步计费并登记准备任务，IO 随部署 worker 执行（契约 §3.3）
    fn try_freeze(&self, source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError>;
    /// 同步入队**已冻结**事件；预算身份与部署预算逐项核对（契约 §3.6）
    fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError>;
    /// 部署预算身份；构造 `TelemetryEvent` 只允许使用同一份预算，不允许替换
    fn memory_budget(&self) -> &Arc<MemoryBudget>;
    /// 异步 flush 待发送事件，报告本次捕获前缀的交付结果
    fn flush(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FlushReport, LangfuseError>> + Send + '_>>;
    /// 获取 session ID
    fn session_id(&self) -> &str;
    /// 查询安全的背压丢弃计数；实现不得返回事件 payload。
    fn drop_registry(&self) -> &LangfuseDropRegistry;
    /// sid → 活跃 turn trace 注册表；指标出口按指标自带的 sid 取活跃 trace。
    fn turn_traces(&self) -> &TurnTraceRegistry;
}
