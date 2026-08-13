//! 控制面（Feature F5）：instance 注册表（生命周期 + 指令下发）、chat 注册表
//! （状态机 + binding + 对账）、hub（装配 + 优雅关闭 + Degraded 入口）、
//! heartbeat（keep_alive）、close_codes（关闭码）（架构 §12 目录结构）。
//!
//! 依赖方向：`control` 依赖 `channel` + `state` + `persist` + `auth`；
//! `hub.rs` 是唯一装配点（设计稿 `f5-channel-control.md` §2）。
//!
//! 设计稿：`docs/plans/f5-channel-control.md` §11–§14；权威：`docs/architecture.md`
//! §4.5–§4.7、§7.1–§7.6、§8.3–§8.6、§9.2、§17.2。

mod chat_registry;
mod close_codes;
mod heartbeat;
mod hub;
mod instance_registry;
mod project_service;
mod workspace_registry;

pub use chat_registry::{ChatError, ChatRecord, ChatRegistry, ChatState, ReconciliationReport};
pub use close_codes::{
    reconnect_policy, ReconnectPolicy, CLOSE_CONFIG_FATAL, CLOSE_INSTANCE_OFFLINE,
    CLOSE_KEEPALIVE_TIMEOUT,
};
pub use heartbeat::{Heartbeat, HeartbeatDriver, HeartbeatOutcome};
pub use hub::{Hub, HubError, StoreSink};
pub use instance_registry::{
    HelloOutcome, InstanceAck, InstanceConn, InstanceError, InstanceRegistry, InstanceState,
    KillOutcome, SpawnOutcome,
};
pub use project_service::{ProjectService, ProjectServiceError};
pub use workspace_registry::{WorkspaceError, WorkspaceRecord, WorkspaceRegistry};
