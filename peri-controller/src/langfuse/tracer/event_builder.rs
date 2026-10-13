//! Langfuse 事件构造基础设施。
//!
//! 统一时间戳生成、UUID 生成、事件入队等重复样板，消除原 tracer.rs 中 15+ 处时间戳
//! 与 10+ 处入队的重复代码。所有方法均为纯函数（接收 owned 数据），
//! 避免与 LangfuseTracer 的可变借用冲突（详见 on_tool_end 借用 workaround）。

use langfuse_client::{IngestionEvent, InputSource, TelemetryEvent};

use super::super::drop_telemetry::{LangfuseDropReason, LangfuseEventKind};

use super::super::session_like::LangfuseSessionLike;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 生成 RFC3339 时间戳（UTC）。
///
/// [不变量] 与 [`peri_time::now_utc_rfc3339`] 同一份实现：观测的 start_time 由各追踪器直接
/// 取自该函数，若此处另用毫秒截断，同一毫秒内结束的观测会得到「结束早于开始」的反转时间，
/// 被 OTLP 校验整条拒绝（契约 §3.3 的结束路径不等 IO，start/end 常落在同一毫秒）。
pub(crate) fn now_rfc3339() -> String {
    peri_time::now_utc_rfc3339()
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

/// 将**已冻结输入来源**的观测事件同步入队 session（契约 §3.3）。
///
/// 与 [`try_add_or_warn_via_session`] 不是双路径：携带 input/output 来源的事件必须在同一
/// 部署预算内构造 [`TelemetryEvent`]（`try_new` 校验来源与事件 permit 的预算身份），因此
/// 只有本入口可用。构造或准入失败时事件与来源一起释放（ticket Drop 置取消位），
/// 不入队半成品正文；背压原因照旧计数，其余错误作安全诊断。
pub(crate) fn try_add_prepared_or_warn_via_session(
    session: &dyn LangfuseSessionLike,
    event: IngestionEvent,
    input: Option<InputSource>,
    output: Option<InputSource>,
    trace_id: &str,
    context_msg: &str,
) {
    let event_kind = LangfuseEventKind::from_event(&event);
    let queued = TelemetryEvent::try_new(event, input, output, session.memory_budget())
        .and_then(|event| session.try_add_prepared(event));
    if let Err(error) = queued {
        if let Some(reason) = LangfuseDropReason::from_error(&error) {
            session.drop_registry().record(trace_id, event_kind, reason);
        } else {
            tracing::warn!(
                target: "langfuse::drop",
                trace_id = %trace_id,
                event_kind = ?event_kind,
                "{} was not queued",
                context_msg
            );
        }
    }
}

#[cfg(test)]
#[path = "event_builder_test.rs"]
mod tests;
