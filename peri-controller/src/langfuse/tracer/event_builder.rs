//! Langfuse 事件构造基础设施。
//!
//! 统一时间戳生成、UUID 生成、事件入队等重复样板，消除原 tracer.rs 中 15+ 处时间戳
//! 与 10+ 处入队的重复代码。所有方法均为纯函数（接收 owned 数据），
//! 避免与 LangfuseTracer 的可变借用冲突（详见 on_tool_end 借用 workaround）。

use langfuse_client::IngestionEvent;

use super::super::drop_telemetry::{LangfuseDropReason, LangfuseEventKind};

use super::super::session_like::LangfuseSessionLike;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 生成 RFC3339 时间戳（毫秒精度，UTC）。
///
/// 统一原文件中 `chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)`
/// 的 15+ 处重复调用。
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// 生成 UUID v7 字符串。
pub(crate) fn new_uuid() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// 将事件同步入队 batcher，背压诊断由 batcher 汇总。
///
/// [不变量] 同步入队保留同一生产路径的调用顺序及 FIFO 屏障语义。
/// 父 observation ID 在生命周期内保持稳定，父观测可延迟到终态导出；
/// 不承诺父观测先于子观测导出，也不承诺并发 HTTP 请求的到达顺序。
/// try_add 失败时背压丢弃且不阻断。禁止改为 panic 或返回 Result 中断追踪。
pub(crate) fn try_add_or_warn(
    batcher: &langfuse_client::Batcher,
    event: IngestionEvent,
    trace_id: &str,
    context_msg: &str,
) {
    let event_kind = LangfuseEventKind::from_event(&event);
    if let Err(error) = batcher.try_add(event) {
        if LangfuseDropReason::from_error(&error).is_none() {
            tracing::warn!(target: "langfuse::drop", trace_id = %trace_id, event_kind = ?event_kind, "{} was not queued", context_msg);
        }
    }
}

/// 将事件同步入队 session，背压仅计数，未知错误作安全诊断。
///
/// [不变量] 同步入队保留同一生产路径的调用顺序及 FIFO 屏障语义，父 ID 保持稳定。
/// 父观测可延迟导出，不承诺父先于子导出或并发 HTTP 请求的到达顺序。
pub(crate) fn try_add_or_warn_via_session(
    session: &dyn LangfuseSessionLike,
    event: IngestionEvent,
    trace_id: &str,
    _context_msg: &str,
) {
    let event_kind = LangfuseEventKind::from_event(&event);
    if let Err(error) = session.try_add(event) {
        if let Some(reason) = LangfuseDropReason::from_error(&error) {
            session.drop_registry().record(trace_id, event_kind, reason);
        } else {
            tracing::warn!(
                target: "langfuse::drop",
                trace_id = %trace_id,
                "langfuse event was not queued"
            );
        }
    }
}

#[cfg(test)]
#[path = "event_builder_test.rs"]
mod tests;
