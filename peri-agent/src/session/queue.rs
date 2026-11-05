//! MessageQueue v2 — 会话级临时收件箱（事实源 `peri-acp-types::session`，
//! 本模块 re-export 保兼容）。
//!
//! 消息分三类（Prompt/Defer/Info）控制循环唤醒与消费行为；RCRA 循环的
//! Receive 阶段通过 `drain_all` 一次性消费，循环退出后 `has_wake_up` 检测激活。

pub use peri_acp_types::session::{
    ExecutionBinding, MessageActivation, MessageDisposition, MessageKind, MessagePolicy,
    MessageQueue, MessageRequirement, MessageSource, ProducerQueue, QueuedMessage, QueuedPayload,
};

/// 同步能力面的入队：等到真实持久提交后返回结论。
///
/// 中间件 hook 与后台完成回调都还是同步 `fn`，但投递结果必须是可观察的事实：
/// 内存存储就地提交，真实存储用独立 runtime 等到写入结束，失败原样交给生产者重试，
/// 不允许「先返回成功、再 detached 写入」。
pub(crate) fn enqueue_blocking(
    producer: ProducerQueue<'_>,
    message: QueuedMessage,
) -> Result<(), String> {
    if let Some(result) = producer.enqueue_now(message.clone()) {
        return result.map(|_| ()).map_err(|error| error.to_string());
    }
    let producer = producer.to_handle();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let bridge = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("MQ enqueue bridge runtime: {error}"))?;
                bridge.block_on(async move {
                    producer
                        .enqueue(message)
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
            })
            .join()
            .map_err(|_| "MQ enqueue bridge thread panicked".to_string())?
    })
}

/// 同步中间件能力面的过渡桥。
///
/// 中间件 hook 的窄接口仍是同步 `fn`，而 MQ 入队必须 await 持久提交。这里把
/// 提交投递到当前 runtime；调用方拿不到“已持久”的结论，失败只留日志。
/// `docs/design/thread-mq.md` §4.1 要求业务方经统一接口入队，待中间件能力
/// 改为 async 后删除本桥。
pub(crate) fn enqueue_detached(producer: ProducerQueue<'_>, message: QueuedMessage) {
    if let Some(result) = producer.enqueue_now(message.clone()) {
        if let Err(error) = result {
            tracing::warn!(%error, "middleware MQ enqueue failed");
        }
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // 在途凭证随写入任务结束释放；Receive 会等计数归零再选择前缀。
            let pending = producer.begin_detached_enqueue();
            let producer = producer.to_handle();
            handle.spawn(async move {
                let _pending = pending;
                if let Err(error) = producer.enqueue(message).await {
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
