//! 通道与发送/接收句柄；保持各层现有容量、丢弃和 lagging 行为。

use super::{ObserveEvent, RenderEvent, StateEvent};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

/// 单次发送结果；调用方可按事件的重要性选择处理，不强制消费返回值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitOutcome {
    Sent,
    Dropped,
    Closed,
    Lagging,
}

/// 进程内事件丢失诊断；dirty 为 sticky，成功发送或补发局部快照不会清除。
#[derive(Debug, Default)]
pub struct BusHealth {
    render_dropped: AtomicU64,
    state_dropped: AtomicU64,
    dirty: AtomicBool,
}

/// 累积观测，不是跨字段事务快照，也不证明源端或客户端完整交付。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BusHealthSnapshot {
    pub render_dropped: u64,
    pub state_dropped: u64,
    pub dirty: bool,
}

impl BusHealth {
    pub fn snapshot(&self) -> BusHealthSnapshot {
        let render_dropped = self.render_dropped.load(Ordering::Acquire);
        let state_dropped = self.state_dropped.load(Ordering::Acquire);
        BusHealthSnapshot {
            render_dropped,
            state_dropped,
            dirty: self.dirty.load(Ordering::Acquire) || render_dropped != 0 || state_dropped != 0,
        }
    }

    pub fn record_render(&self, outcome: EmitOutcome, count: u64) {
        self.record(&self.render_dropped, outcome, count);
    }

    /// 混合广播无法恢复已跳过事件的类别，统一计入 state_dropped。
    pub fn record_state(&self, outcome: EmitOutcome, count: u64) {
        self.record(&self.state_dropped, outcome, count);
    }

    fn record(&self, counter: &AtomicU64, outcome: EmitOutcome, count: u64) {
        if outcome != EmitOutcome::Sent {
            self.dirty.store(true, Ordering::Release);
            counter.fetch_add(count, Ordering::AcqRel);
        }
    }
}

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
    health: Arc<BusHealth>,
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
            health: Arc::new(BusHealth::default()),
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
    pub fn emit_render(&self, event: RenderEvent) -> EmitOutcome {
        let outcome = match self.render_tx.try_send(event) {
            Ok(()) => EmitOutcome::Sent,
            Err(mpsc::error::TrySendError::Full(_)) => EmitOutcome::Dropped,
            Err(mpsc::error::TrySendError::Closed(_)) => EmitOutcome::Closed,
        };
        self.health.record_render(outcome, 1);
        if outcome != EmitOutcome::Sent {
            tracing::warn!(event = "render_event_dropped", ?outcome, "渲染层事件未投递");
        }
        outcome
    }

    /// 发送必须保序、不可丢弃的渲染事件（如用户输入交付）。
    ///
    /// 与 [`Self::emit_render`] 共用同一条 FIFO：进入顺序即 forwarder 的消费
    /// 顺序，`session_seq` 在消费时按该顺序补打。通道满时等待消费者腾出容量，
    /// 不降级丢弃——交付事件一旦丢失，用户消息可能落到本轮回答之后。
    pub async fn emit_render_reliable(&self, event: RenderEvent) -> EmitOutcome {
        if self.render_tx.send(event).await.is_err() {
            self.health.record_render(EmitOutcome::Closed, 1);
            tracing::warn!(
                event = "render_event_receiver_closed",
                "渲染层接收端已关闭，可靠渲染事件丢弃"
            );
            return EmitOutcome::Closed;
        }
        EmitOutcome::Sent
    }

    /// 发送状态层事件（critical，满时降级丢弃）。
    pub fn emit_state(&self, event: StateEvent) -> EmitOutcome {
        let outcome = match self.state_tx.try_send(event) {
            Ok(()) => EmitOutcome::Sent,
            Err(mpsc::error::TrySendError::Full(_)) => EmitOutcome::Dropped,
            Err(mpsc::error::TrySendError::Closed(_)) => EmitOutcome::Closed,
        };
        self.health.record_state(outcome, 1);
        if outcome != EmitOutcome::Sent {
            tracing::warn!(event = "state_event_dropped", ?outcome, "状态层事件未投递");
        }
        outcome
    }

    /// 与普通状态事件共用 FIFO；满时等待，关闭时记录丢失并返回 Closed。
    pub async fn emit_state_reliable(&self, event: StateEvent) -> EmitOutcome {
        if self.state_tx.send(event).await.is_err() {
            self.health.record_state(EmitOutcome::Closed, 1);
            tracing::warn!(
                event = "state_event_receiver_closed",
                "状态层接收端已关闭，可靠状态事件丢弃"
            );
            return EmitOutcome::Closed;
        }
        EmitOutcome::Sent
    }

    pub fn health(&self) -> BusHealthSnapshot {
        self.health.snapshot()
    }

    pub fn health_handle(&self) -> Arc<BusHealth> {
        Arc::clone(&self.health)
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
