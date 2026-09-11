//! 通道与发送/接收句柄；保持各层现有容量、丢弃和 lagging 行为。

use super::{ObserveEvent, RenderEvent, StateEvent};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

// ─── EventBus（生产端） ───────────────────────────────────────────────────────

/// 事件总线 — 生产端，持有三个通道的 Sender
///
/// - 渲染层 / 状态层：`tokio::sync::mpsc` 有界通道，`try_send` 满时降级丢弃
/// - 观测层：`tokio::sync::broadcast` 通道，慢消费者自动 lagging
///
/// 通道容量通过 `EventBus::new()` 的参数配置。
pub struct EventBus {
    render_tx: mpsc::Sender<RenderEvent>,
    state_tx: mpsc::Sender<StateEvent>,
    observe_tx: broadcast::Sender<ObserveEvent>,
    /// 兼容保留的配置值；当前发送失败后不会等待或重试。
    _drop_timeout: Duration,
}

/// EventBus 构建参数
#[derive(Debug, Clone)]
pub struct EventBusConfig {
    /// 渲染层通道容量（默认 256）
    pub render_capacity: usize,
    /// 状态层通道容量（默认 64）
    pub state_capacity: usize,
    /// 观测层 broadcast 通道容量（默认 128）
    pub observe_capacity: usize,
    /// 兼容配置（默认 50ms）；当前实现不使用该值等待或重试。
    pub drop_timeout: Duration,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            render_capacity: 256,
            state_capacity: 64,
            observe_capacity: 128,
            drop_timeout: Duration::from_millis(50),
        }
    }
}

impl EventBus {
    /// 创建 EventBus，返回 (EventBus, EventHandles)
    ///
    /// `EventBus` 给生产者（Agent），`EventHandles` 给消费者（TUI / 遥测）。
    pub fn new(config: EventBusConfig) -> (Self, EventHandles) {
        let (render_tx, render_rx) = mpsc::channel(config.render_capacity);
        let (state_tx, state_rx) = mpsc::channel(config.state_capacity);
        let (observe_tx, observe_rx) = broadcast::channel(config.observe_capacity);

        let bus = Self {
            render_tx,
            state_tx,
            observe_tx,
            _drop_timeout: config.drop_timeout,
        };

        let handles = EventHandles {
            render_rx,
            state_rx,
            observe_rx,
        };

        (bus, handles)
    }

    /// 发送渲染层事件（critical，满时降级丢弃）
    pub fn emit_render(&self, event: RenderEvent) {
        // 有界通道 + try_send：满时丢弃，不阻塞循环
        if self.render_tx.try_send(event).is_err() {
            tracing::warn!(event = "render_event_dropped", "渲染层通道已满，事件丢弃");
        }
    }

    /// 发送状态层事件（critical，满时降级丢弃）
    pub fn emit_state(&self, event: StateEvent) {
        if self.state_tx.try_send(event).is_err() {
            tracing::warn!(event = "state_event_dropped", "状态层通道已满，事件丢弃");
        }
    }

    /// 发送观测层事件（broadcast，慢消费者自动跳过）
    ///
    /// 返回接收者数量（0 表示无订阅者）。
    pub fn emit_observe(&self, event: ObserveEvent) -> usize {
        match self.observe_tx.send(event) {
            Ok(n) => n,
            Err(_) => {
                tracing::debug!(event = "observe_event_no_subscriber", "观测层无订阅者");
                0
            }
        }
    }
}

// ─── EventHandles（消费端） ───────────────────────────────────────────────────

/// 事件句柄 — 消费端，持有三个通道的 Receiver
///
/// 可直接消费各 Receiver，或使用 `try_render` / `try_state` / `try_observe`。
pub struct EventHandles {
    pub render_rx: mpsc::Receiver<RenderEvent>,
    pub state_rx: mpsc::Receiver<StateEvent>,
    pub observe_rx: broadcast::Receiver<ObserveEvent>,
}

impl EventHandles {
    /// 非阻塞获取下一个渲染层事件
    pub fn try_render(&mut self) -> Option<RenderEvent> {
        self.render_rx.try_recv().ok()
    }

    /// 非阻塞获取下一个状态层事件
    pub fn try_state(&mut self) -> Option<StateEvent> {
        self.state_rx.try_recv().ok()
    }

    /// 非阻塞获取下一个观测层事件（lagging 时返回 None）
    pub fn try_observe(&mut self) -> Option<ObserveEvent> {
        self.observe_rx.try_recv().ok()
    }

    /// 订阅观测层（创建新的 Receiver，共享同一 broadcast 通道）
    ///
    /// 用于多个独立消费者同时订阅观测层。
    pub fn subscribe_observe(&self) -> broadcast::Receiver<ObserveEvent> {
        self.observe_rx.resubscribe()
    }
}

#[cfg(test)]
#[path = "bus_test.rs"]
mod tests;
