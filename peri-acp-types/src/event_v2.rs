//! v2 事件契约的公共入口；Agent 与 ACP 保留对本模块的 re-export。
//!
//! 事件载荷强制携带 turn_id 和 agent_id，通道与协议转换分别由私有模块维护。
//! Render（包含 TurnCompleted）与 State 使用有界 mpsc，try_send 满时立即丢弃；
//! Observe 使用有界 broadcast，慢消费者可能 lag。通道配置的 drop_timeout
//! 兼容保留，当前不参与重试或等待。
//! *_event_to_executor 是共享协议兼容转换；消费者的身份补充与生命周期
//! 决策仍在各自 forwarder，不在类型契约层复制状态。

mod bus;
mod executor_mapping;
mod types;

pub use bus::{EventBus, EventBusConfig, EventHandles};
pub use executor_mapping::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};
pub use types::{Event, ObserveEvent, RenderEvent, StateEvent, TurnErrorReason};
