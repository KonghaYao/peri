//! 执行编排薄壳（3.0 批 2 归位）。
//!
//! `run_session_loop` 及其执行子流程（`build_and_execute_agent_v2` /
//! `build_stage_context` / `spawn_eventbus_forwarder` / workflow agent 执行器）
//! 已归位到 `crate::host::exec`（过渡宿主：深绑 Agent 层执行类型，全路径引用，
//! 豁免至 L5——见 `spec/issues/2026-08-05-3.0-acp-events-session-batch2.md`；
//! 随 executor 拆分物理迁入 peri-agent）。
//!
//! 本模块保留共享类型与入口的协议化路径（EventSink / Langfuse 观测 /
//! SessionManager 编排均在 ACP 层），执行细节在 host::exec。

pub(crate) use crate::host::exec::executor::PERMISSION_MODE_NEVER_NOTIFIED;
pub use crate::host::exec::executor::{
    execute_prediction, extract_prediction_text, is_keepgoing, parse_prediction_actions,
    run_session_loop, ContinuationRequest, FrozenSessionData, PredictionError, PromptResult,
    PromptStopReason, SessionContext, TurnInput,
};
