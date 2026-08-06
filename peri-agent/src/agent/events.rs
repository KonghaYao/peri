//! v1 事件载荷类型 re-export（事实源 `peri-acp-types::event`）。
//!
//! v1 `ExecutorEvent` 中间态已退役（`2026-07-18-executor-event-retirement.md`，
//! 批 2「v1-retire」）：peri-agent 内部发射统一 v2 形态（`events_v2` 三层事件，
//! ObserveEvent 身份透传），`ExecutorEvent` 仅保留为 ACP 协议序列化面需要的
//! 最小映射载体（定义与 `*_event_to_executor` 映射留在 `peri-acp-types`）。
//! 本模块 re-export 仅供 Agent 层协议化边界（`AgentEventHandler` / bg 泵通道）
//! 与兼容测试使用，不承载业务事件发射。

pub use peri_acp_types::event::{
    AgentEventHandler, BackgroundTaskResult, CompactFileInfo, CompactStrategy, CompactThreshold,
    CompactTrigger, ExecutorEvent, FnEventHandler, MiddlewareHook, Stage, StageStatus, TodoEntry,
    TodoStatus, TurnErrorKind, TurnStatus, WorkflowProgressPayload,
};
