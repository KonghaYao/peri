//! 全局 v2 事件直连通道——forwarder 扇出原事件给 TUI 直接消费。
//!
//! TUI 启动时调用 `set_v2_event_tx` 注册 sender。forwarder 每收到一个 v2 事件
//! 就调用 `try_send_v2_event` 无阻塞投递。如果 TUI 未启动（通道未注册）或通道关闭，
//! 静默忽略。

use peri_agent::agent::events_v2_mapper::V2Event;
use std::sync::OnceLock;
use tokio::sync::mpsc;

static V2_EVENT_TX: OnceLock<mpsc::UnboundedSender<V2Event>> = OnceLock::new();

/// TUI entry 启动时调用一次。只能设置一次，重复调用 panic（callback）。
pub fn set_v2_event_tx(tx: mpsc::UnboundedSender<V2Event>) {
    V2_EVENT_TX
        .set(tx)
        .expect("v2_event_tx 已注册（set_v2_event_tx 只能调用一次）");
}

/// 无阻塞发送 v2 事件。如果 sender 未注册（TUI 未启动）或通道关闭，静默忽略。
pub fn try_send_v2_event(event: V2Event) {
    if let Some(tx) = V2_EVENT_TX.get() {
        // try_send 不阻塞：forwarder 在 async context 中，不可 await send
        let _ = tx.send(event);
    }
}
