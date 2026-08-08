//! 通道层（Feature F5）：gateway（ws 生命周期）、session-channel（客户端连接
//! 归一化）、command-coordinator（串行队列 + commandId 去重持久化）、
//! relay-event-handler（machine 入站消费与断链清理）、broadcaster（fan-out +
//! 背压）、connection-registry（配额）（架构 §12 目录结构）。
//!
//! 依赖方向（单向，防环）：`protocol`（纯函数）← `channel`（依赖 protocol +
//! state + persist + auth）← `control`（装配）。模块间句柄经 `Arc<dyn Trait>`
//! /struct 引用注入（设计稿 `f5-channel-control.md` §2 句柄表）。
//!
//! 设计稿：`docs/plans/f5-channel-control.md` §5–§10；权威：`docs/architecture.md`
//! §4.2–§4.8、§6、§7.4、§8、§9。

mod broadcaster;
mod command_coordinator;
mod connection_registry;
mod gateway;
mod relay_event_handler;
mod session_channel;

pub use broadcaster::{
    BackpressureAction, Broadcaster, OutboundMsg, SubError, decide_backpressure,
};
pub use command_coordinator::{CommandCoordinator, ExecCmd, SubmitAck};
pub use connection_registry::{ConnHandle, ConnId, ConnectionRegistry, RegistryFull};
pub use gateway::{Gateway, GatewayError};
pub use relay_event_handler::{ConsumeResult, PendingRpc, RelayError, RelayEventHandler};
pub use session_channel::{ChannelDeps, DispatchOutcome, SessionChannel};
