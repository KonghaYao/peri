//! Compact event handlers — CompactStarted, CompactCompleted.
//!
//! Phase 5 Step 4/7：CompactCompleted 收敛为「状态重建信号」——仅保留
//! `trigger=="manual"` 置 `compact_just_completed`（R3 标志链：TurnDone →
//! session/load 重放）；SystemNote 文案注入已删除（通知文案移交
//! CommandFeedback 事件渲染，历史回看依赖 TUI 事件日志——设计文档 §81）。
//! CompactError 变体与 handler 已删除（错误反馈收敛到 CommandFeedback）。

use super::*;

pub(super) fn handle_compact_started(state: &mut BridgeState) {
    tracing::info!("bridge: CompactStarted");
    state.phase = SessionPhase::PromptRunning;
    super::render::push_acp_state(state);
}

pub(super) fn handle_compact_completed(state: &mut BridgeState, trigger: &str) {
    tracing::info!(%trigger, "bridge: CompactCompleted");
    // S4.1 方案 A：trigger 由服务端透传。仅手动 /compact 置
    // compact_just_completed（TurnDone 需在完整重建后到达）；auto compact
    // 不置位——auto 后 ReAct 循环继续运行，zero-output 后重放旧消息的
    // 边缘洞即被根治（流事件清除逻辑保留为防御）。
    if trigger == "manual" {
        state.compact_just_completed = true;
    }
    // 不重置 phase——auto compact 后 ReAct 循环继续运行，
    // loading 由流式事件（TextChunk/ToolStarted）和 TurnDone 管理。
    // 手动 /compact 路径由 push_done → TurnDone 兜底清除。
}
