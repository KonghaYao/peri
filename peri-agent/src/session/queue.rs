//! MessageQueue v2 — 会话级临时收件箱（事实源 `peri-acp-types::session`，
//! 本模块 re-export 保兼容）。
//!
//! 消息分三类（Prompt/Defer/Info）控制循环唤醒与消费行为；RCRA 循环的
//! Receive 阶段通过 `drain_all` 一次性消费，循环退出后 `has_wake_up` 检测激活。

pub use peri_acp_types::session::{
    ExecutionBinding, MessageActivation, MessageDisposition, MessageKind, MessagePolicy,
    MessageQueue, MessageRequirement, MessageSource, QueuedMessage, QueuedPayload,
};

/// 同步中间件能力面的过渡桥。
///
/// 中间件 hook 的窄接口仍是同步 `fn`，而 MQ 入队必须 await 持久提交。这里把
/// 提交投递到当前 runtime；调用方拿不到“已持久”的结论，失败只留日志。
/// `docs/design/thread-mq.md` §4.1 要求业务方经统一接口入队，待中间件能力
/// 改为 async 后删除本桥。
pub(crate) fn enqueue_detached(queue: &MessageQueue, message: QueuedMessage) {
    let queue = queue.clone();
    if let Some(result) = queue.enqueue_now(message.clone()) {
        if let Err(error) = result {
            tracing::warn!(%error, "middleware MQ enqueue failed");
        }
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                if let Err(error) = queue.enqueue(message).await {
                    tracing::warn!(%error, "middleware MQ enqueue failed");
                }
            });
        }
        Err(_) => {
            tracing::warn!("middleware MQ enqueue skipped: no active Tokio runtime");
        }
    }
}

#[cfg(test)]
#[path = "queue_test.rs"]
mod tests;
