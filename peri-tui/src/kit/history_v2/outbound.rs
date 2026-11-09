//! History V2 连接级出站调度。
//!
//! 所有 V2 RPC（`session/load` initial、open/page/seek/body/close）都进入同一个
//! FIFO 队列；消费者任务严格按入队顺序发送。requestKey 的分配与入队必须在同一
//! 临界区内完成：原子分配只能保证编号唯一，不能保证“先分配的编号先上线”。
//!
//! 窗口类请求由消费者结算本地状态；body / load 只负责发送，把原始响应经 oneshot
//! 回传给调用方（详情面板 / `session/load` 生命周期），调用方各自保留超时、
//! 在途标记与严格解码。

use std::sync::Mutex;

use peri_acp::transport::types::AcpError;
use peri_acp_types::history_v2::DecimalU64;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::kit::atoms;

use super::consumer::{HistoryV2ConsumerRequest, next_request_key};
use super::request::HistoryV2BodyRequest;

/// 出站命令：窗口操作、详情正文、会话首屏。
pub(crate) enum OutboundCommand {
    /// open/page/seek/close：消费者发送并结算 `HISTORY_V2_STATE`。
    Request(HistoryV2ConsumerRequest),
    /// 详情正文：只发送并回传原始响应，不写窗口账目。
    Body {
        params: Value,
        reply: oneshot::Sender<Result<Value, AcpError>>,
    },
    /// `session/load`：只发送并回传原始响应，生命周期仍由 `session.rs` 负责。
    Load {
        params: Value,
        reply: oneshot::Sender<Result<Value, AcpError>>,
    },
}

/// requestKey 分配 + 入队的唯一临界区。
static ENQUEUE_ORDER: Mutex<()> = Mutex::new(());

fn enqueue_guard() -> std::sync::MutexGuard<'static, ()> {
    ENQUEUE_ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sender() -> Option<mpsc::UnboundedSender<OutboundCommand>> {
    atoms::history_v2_tx()
}

/// 窗口请求的入队结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowEnqueue {
    /// 已入队；携带最终写进 wire 的 requestKey。
    Sent(DecimalU64),
    /// 构造阶段拒绝（如窗口忙、状态未就绪），未入队。
    Rejected,
    /// 没有安装调度器（headless / 单元测试），调用方不得绕过队列伪造顺序。
    Unavailable,
    /// 调度器已关闭；携带已分配的 key，供调用方回滚本地认领。
    Closed(DecimalU64),
}

/// 在统一临界区内分配 key、构造请求并入队。
pub(crate) fn enqueue_window<F>(build: F) -> WindowEnqueue
where
    F: FnOnce(DecimalU64) -> Option<HistoryV2ConsumerRequest>,
{
    let _order = enqueue_guard();
    let Some(tx) = sender() else {
        return WindowEnqueue::Unavailable;
    };
    let key = next_request_key();
    let Some(request) = build(key) else {
        return WindowEnqueue::Rejected;
    };
    match tx.send(OutboundCommand::Request(request)) {
        Ok(()) => WindowEnqueue::Sent(key),
        Err(_) => WindowEnqueue::Closed(key),
    }
}

/// body / load 的提交结果：有调度器时排队等待 oneshot；没有调度器时返回已构造
/// 的请求，由调用方直接发送（此时不存在任何队列，因而不存在队列重排）。
pub(crate) enum Submit<T> {
    Queued(oneshot::Receiver<Result<Value, AcpError>>),
    Direct(T),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmitError {
    /// 调度器存在但已关闭；不得绕过队列直发。
    Closed,
}

/// 提交一次详情正文读取。
pub(crate) fn submit_body<F>(build: F) -> Result<Submit<HistoryV2BodyRequest>, SubmitError>
where
    F: FnOnce(DecimalU64) -> HistoryV2BodyRequest,
{
    let _order = enqueue_guard();
    let key = next_request_key();
    let request = build(key);
    let Some(tx) = sender() else {
        return Ok(Submit::Direct(request));
    };
    let (reply, receiver) = oneshot::channel();
    tx.send(OutboundCommand::Body {
        params: request.params(),
        reply,
    })
    .map_err(|_| SubmitError::Closed)?;
    Ok(Submit::Queued(receiver))
}

/// 提交一次 `session/load`；同时返回实际写入 wire 的 requestKey，供响应回显校验。
pub(crate) fn submit_load<F>(build: F) -> Result<(DecimalU64, Submit<Value>), SubmitError>
where
    F: FnOnce(DecimalU64) -> Value,
{
    let _order = enqueue_guard();
    let key = next_request_key();
    let params = build(key);
    let Some(tx) = sender() else {
        return Ok((key, Submit::Direct(params)));
    };
    let (reply, receiver) = oneshot::channel();
    tx.send(OutboundCommand::Load { params, reply })
        .map_err(|_| SubmitError::Closed)?;
    Ok((key, Submit::Queued(receiver)))
}
