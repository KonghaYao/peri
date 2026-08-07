use langfuse_client::{IngestionEvent, LangfuseError};
use std::future::Future;
use std::pin::Pin;

use super::drop_telemetry::LangfuseDropRegistry;

/// Langfuse session 抽象，让 tracer 可注入 fake session 跑单测。
pub trait LangfuseSessionLike: Send + Sync {
    /// 同步添加事件到批量队列
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError>;
    /// 异步 flush 待发送事件
    fn flush(&self) -> Pin<Box<dyn Future<Output = Result<(), LangfuseError>> + Send + '_>>;
    /// 获取 session ID
    fn session_id(&self) -> &str;
    /// 查询安全的背压丢弃计数；实现不得返回事件 payload。
    fn drop_registry(&self) -> &LangfuseDropRegistry;
}
