//! v1 事件中间态与载荷类型（事实源 `peri-acp-types::event`，本模块 re-export 保兼容）。
//!
//! v1 `ExecutorEvent` 中间态尚未全量退役（依赖 StdioEventSink 迁移，
//! `2026-07-18-executor-event-retirement.md`），v2 事件经 `events_v2` 的
//! v1 兼容映射转换为本类型后由 peri-acp 协议化。定义随 ExecutorEvent
//! 全量退役一起删除。

pub use peri_acp_types::event::{
    AgentEventHandler, BackgroundTaskResult, CompactFileInfo, CompactStrategy, CompactThreshold,
    CompactTrigger, ExecutorEvent, FnEventHandler, MiddlewareHook, Stage, StageStatus, TodoEntry,
    TodoStatus, TurnErrorKind, TurnStatus, WorkflowProgressPayload,
};
