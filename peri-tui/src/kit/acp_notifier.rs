//! ACP notifier——AcpNotification → AcpEventData 转换器。
//!
//! 直接在 notifier 内完成 DTO 转换，产出的 `AcpEventData` 立即送入 `spawn_acp_bridge`。
//! - **以 session/update 为流式主通道**：ACP 服务端的高频流式事件
//!   （agent_message_chunk / agent_thought_chunk / tool_call / tool_call_update）
//!   通过标准 `session/update` 携带，在 `handle_session_update` 中转换为
//!   `AcpEventData` 变体推入双 bridge channel。
//! - **usage_update**：token 消耗通过标准 session/update 的 `usage_update` tag
//!   携带；root usage 更新 spinner，并把本次请求的 cache observation（包括显式
//!   零命中）转为 session-enveloped `CacheUsageUpdated`；bridge 在每次 root
//!   usage_update 上计算覆盖率并在开启配置时注入警告。
//!   auxiliary usage 不产生父 turn 的 cache 提示。
//! - **AgentEvent DTO 已接入**：`peri/agent_event` 携带的 AcpEvent 变体
//!   （SubagentStarted/SubagentStopped/TurnSuspended/RewindCompleted/...）
//!   通过 `convert_agent_event` 转换为 AcpEventData 推入双 bridge channel。
//!   未映射变体（StateSnapshot/BgToolStep/ContextWarning/...）
//!   保持静默丢弃，S5+ 迭代扩展。
//!
//! 私有模块解码 DTO；本任务顺序发布 commands/plan/spinner 状态后推入 bridge，
//! 并在 transport 关闭时复位连接相关 UI 状态。

mod agent_event;
mod interaction;
mod session_update;

#[cfg(test)]
#[path = "acp_notifier_test.rs"]
mod tests;

#[cfg(test)]
#[path = "acp_notifier_agent_test.rs"]
mod agent_tests;

#[cfg(test)]
#[path = "acp_notifier_lifecycle_test.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "acp_notifier_projection_test.rs"]
mod projection_tests;

#[cfg(test)]
#[path = "acp_notifier_interaction_test.rs"]
mod interaction_tests;

#[cfg(test)]
use crate::acp_client::AcpNotification;
#[cfg(test)]
use crate::kit::acp_types::AcpEventData;
#[cfg(test)]
use crate::kit::acp_types::AcpEventWithEpoch;

#[cfg(test)]
use tokio::sync::mpsc;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

mod notifier;
pub use notifier::*;

#[cfg(test)]
use crate::kit::atoms::ACP_STATE;

#[cfg(test)]
use crate::kit::atoms::AVAILABLE_SLASH_COMMANDS;

#[cfg(test)]
use crate::kit::atoms::INPUT_BUFFER;

#[cfg(test)]
use crate::kit::atoms::NOTIFICATION;

#[cfg(test)]
use crate::kit::atoms::RENDER_HEARTBEAT;
