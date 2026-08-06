//! peri-controller — Controller 层（控制面宿主）。
//!
//! 当前承载：
//! - `controller` — 控制面宿主（`docs/top-level.md` §6）：
//!   lite params → pick Resources → pick Runtime → run Session → pop events；
//!   会话生命周期面（join / 销毁 / 枚举）、消息/工具注入面（submit_input）；
//!   cancel 按 (session_id, turn_id, attempt_id) 三元组定位并转发（§9）；
//!   事件协议化前分支（`subscribe` / `pop_events`）供 ACP 协议化与旁路观测
//! - `error` — 边界错误（ControllerError / SubscriptionError，thiserror 枚举，
//!   包 Runtime context）
//! - `langfuse` — 观测实现（自 peri-acp 迁入）：bridge 是事件流旁路消费者，
//!   装配在 Controller 侧宿主，不承担 Controller 职责；事件在协议化前分支给 bridge
//!
//! 依赖方向（§0）：Controller → Runtime / Controller → Resources / 契约层
//! （peri-acp-types）；不依赖 peri-acp。`peri-agent` / `peri-model` 为 langfuse
//! 与工具定义（`ToolDefinition`，LiteParams 注入面）过渡依赖（§0 未声明边，
//! L4 langfuse-bypass-consumer 解耦后随工具定义来源裁定移除）。

pub mod controller;
pub mod error;
pub mod langfuse;

pub use controller::{AgentRef, Controller, LiteParams, Subscription};
pub use error::{ControllerError, SubscriptionError};
