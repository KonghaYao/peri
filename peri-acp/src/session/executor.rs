//! 执行编排薄壳（3.0 批 2 归位 + L5 迁出 + l5-shell 拆桥）。
//!
//! `run_session_loop` 及其执行子流程（`build_and_execute_agent_v2` /
//! `build_stage_context` / `spawn_eventbus_forwarder` / workflow agent 执行器）
//! 已随 L5 物理迁入 `peri-agent::session::exec`（依赖反转：provider /
//! peri_config / AgentPool / SessionManager / Controller 端口化为投影值 +
//! 注入闭包 + [`SessionAccessPort`] / 事件端口，ACP 宿主装配面
//! `host/prompt.rs` / `host/stdio/session/prompt_exec.rs` 构造；
//! `host/exec/` 过渡宿主已随 l5-shell 拆桥删除，forwarder 归位
//! `event/forwarder.rs`、workflow agent 执行器随 p1-wa 归位
//! `peri-agent::agent::workflow`，ACP 侧保留装配面薄壳
//! `host/workflow_agent.rs`）。
//!
//! 本模块保留共享类型与入口的协议化路径（EventSink / Langfuse 观测 /
//! SessionManager 编排均在 ACP 层），执行细节在 peri-agent。

pub use peri_agent::session::exec::executor::{
    execute_prediction, extract_prediction_text, is_keepgoing, parse_prediction_actions,
    run_session_loop, AutoClassifierFactory, ContinuationRequest, FrozenFallbackBuilder,
    FrozenSessionData, LangfuseHooks, LangfuseTurnEndHook, PredictionError, PromptResult,
    PromptStopReason, SessionContext, SubagentLlmFactory, TurnInput,
    PERMISSION_MODE_NEVER_NOTIFIED,
};
