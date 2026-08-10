//! 控制面（Feature F5）：instance 注册表（生命周期 + 指令下发）、chat 注册表
//! （状态机 + binding + 对账）、hub（装配 + 优雅关闭 + Degraded 入口）、
//! heartbeat（keep_alive）、close_codes（关闭码）（架构 §12 目录结构）。
//!
//! 依赖方向：`control` 依赖 `channel` + `state` + `persist` + `auth`；
//! `hub.rs` 是唯一装配点（设计稿 `f5-channel-control.md` §2）。
//!
//! 设计稿：`docs/plans/f5-channel-control.md` §11–§14；权威：`docs/architecture.md`
//! §4.5–§4.7、§7.1–§7.6、§8.3–§8.6、§9.2、§17.2。

mod close_codes;
mod heartbeat;
mod hub;
mod instance_registry;
mod chat_registry;
mod workspace_registry;

pub use close_codes::{
    CLOSE_CONFIG_FATAL, CLOSE_KEEPALIVE_TIMEOUT, CLOSE_INSTANCE_OFFLINE, ReconnectPolicy,
    reconnect_policy,
};
pub use heartbeat::{Heartbeat, HeartbeatDriver, HeartbeatOutcome};
pub use hub::{Hub, HubError, StoreSink};
pub use instance_registry::{
    HelloOutcome, KillOutcome, InstanceAck, InstanceConn, InstanceError, InstanceRegistry,
    InstanceState, SpawnOutcome,
};
pub use chat_registry::{
    ChatError, ChatRecord, ChatRegistry, ChatState, ReconciliationReport,
};
pub use workspace_registry::{WorkspaceError, WorkspaceRecord, WorkspaceRegistry};
