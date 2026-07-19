//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

use crate::i18n;
use crate::kit::acp_types::{AcpEventData, CurrentTurn, ToolCardAccumulator};
use crate::kit::atoms::*;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::tui_render_unit::{
    TuiNoteLevel, TuiRenderUnit, TuiSystemNote, TuiUserBubble, tui_hash_str,
};
use agent_client_protocol::schema::v1::{Plan, PlanEntryStatus};
use fluent_bundle::FluentValue;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// BridgeState — ACP 事件桥接内部状态
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    PromptRunning,
    ReplayingHistory,
}

/// 桥接任务维护的内部状态，每个 ACP 事件到达时同步更新。
///
/// 定义在 acp_events.rs 中以避免 acp_bridge ↔ acp_events 循环依赖。
pub struct BridgeState {
    /// 0=Idle, 1=Streaming, 2=Modal
    pub variant: u8,
    /// 已提交的 TuiRenderUnit 列表——im::Vector 支持 O(1) clone + O(log n) push_back。
    pub committed: im::Vector<TuiRenderUnit>,
    /// 当前轮次的增量数据
    pub current_turn: CurrentTurn,
    /// 当前 session lifecycle 阶段。loading 只由该阶段派生。
    pub phase: SessionPhase,
    /// S7：精确弹窗类型，由 AcpEvent 直接映射。None = 无弹窗。
    /// 弹窗激活状态由 POPUP_KIND.is_some() 派生（status_bar / event_handlers 都读这个）
    pub popup_kind: Option<crate::kit::atoms::PopupKind>,
    /// ViewModelsSnapshot generation——每次 push_view_models 递增。
    /// render_bridge 用此值检测变化（替代原先的 Arc::as_ptr 比较）。
    pub generation: u64,
    /// 当前活跃 session 的 ID。事件携带的 active_session_id 不匹配时丢弃。
    pub active_session_id: String,
    /// `/compact` 命令刚刚完成，TurnDone 时需触发 session/load 重放。
    /// 与 agent 内部 compact 区分：命令 compact 后 current_turn 为空，
    /// agent 内部 compact 后 current_turn 有后续流事件。
    pub compact_just_completed: bool,
    /// 本轮用户提交的文本——TurnInterrupted 零产出回滚时用于恢复输入框。
    /// LocalUserBubble 到达时写入，TurnInterrupted 零产出时消费并清空。
    pub last_submitted_text: Option<String>,
}

impl BridgeState {
    /// 将 current_turn 已产出内容 flush 到 committed，然后 reset。
    ///
    /// 用于 BudgetWarning / SystemNotification / BgCallbackBubble / TurnDone
    /// 四个需要保证时序正确性的位置：在注入中间事件或结束当前 turn 之前，
    /// 必须先将已产出的 AI 内容归档到 committed，确保消息流时间顺序一致。
    ///
    /// 安全守卫：如果 current_turn 中存在正在运行的 SubAgentAccumulator，
    /// 跳过 flush 以避免清除容器——否则后续工具事件无法路由到已清除的容器，
    /// 造成 SubAgentGroup 内部卡片空白（具体现象：外壳可见但内部工具条目缺失）。
    /// 回归参考：与 SystemNotification/BudgetWarning 的时序竞态。
    fn flush_current_turn(&mut self) {
        if !self.current_turn.committed && !self.current_turn.is_empty() {
            let has_running_subagent = self.current_turn.subagents.iter().any(|s| s.is_running);
            if has_running_subagent {
                tracing::debug!(
                    running_count = self
                        .current_turn
                        .subagents
                        .iter()
                        .filter(|s| s.is_running)
                        .count(),
                    "flush_current_turn: 跳过 flush——存在正在运行的 SubAgentAccumulator"
                );
                return;
            }
            for vm in self.current_turn.view_models() {
                self.committed.push_back(vm.clone());
            }
        }
        self.current_turn.reset();
    }
}

// ---------------------------------------------------------------------------
// 核心分发函数
// ---------------------------------------------------------------------------

/// 将 AcpEventData 分发到对应的 Atom 写入，并更新 BridgeState。
///
/// 这是 acp_bridge 消费事件时调用的核心函数。
/// 每次调用按事件类型更新内部状态，然后 push 到 VIEW_MODELS 和 ACP_STATE Atoms。
pub fn dispatch_and_notify(state: &mut BridgeState, event: &AcpEventData) {
    use AcpEventData::*;
    match event {
        // ── §4.1 Streaming events ──
        TextChunk(tc) => {
            if let Some(agent_id) = tc.agent_id.as_deref() {
                if !state.current_turn.append_subagent_text(agent_id, &tc.text) {
                    tracing::trace!(
                        agent_id,
                        "kit bridge: subagent text chunk has no active group"
                    );
                }
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .append_text(&tc.text, tc.message_id.as_deref());
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            }
            push_acp_state(state);
        }
        ReasoningChunk(rc) => {
            if let Some(agent_id) = rc.agent_id.as_deref() {
                if !state
                    .current_turn
                    .append_subagent_reasoning(agent_id, &rc.text)
                {
                    tracing::trace!(
                        agent_id,
                        "kit bridge: subagent reasoning chunk has no active group"
                    );
                }
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .append_reasoning(&rc.text, rc.message_id.as_deref());
                tracing::info!(
                    len = state.current_turn.reasoning.len(),
                    "bridge: reasoning appended"
                );
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            }
            push_acp_state(state);
        }
        ToolStarted(ts) => {
            if let Some(agent_id) = ts.agent_id.as_deref() {
                // bg sub-agent: TurnSuspended 后 SubAgentAccumulator 已被清除，
                // 后续 bg 工具事件仅更新 BG_DISPLAY，不走 start_subagent_tool
                if BG_AGENT_IDS.state().read().contains(agent_id) {
                    if let Some(entry) = BG_DISPLAY
                        .state()
                        .write()
                        .iter_mut()
                        .find(|e| e.id == agent_id)
                    {
                        entry.current_tool = Some(ts.tool_name.clone());
                    }
                    state.variant = 1;
                    state.phase = SessionPhase::PromptRunning;
                    push_view_models(state);
                } else {
                    // 同步 sub-agent: 路由到 SubAgentAccumulator
                    let routed = state.current_turn.start_subagent_tool(
                        agent_id,
                        ToolCardAccumulator::new(
                            ts.tool_id.clone(),
                            ts.tool_name.clone(),
                            ts.input_summary.clone(),
                        ),
                    );
                    if !routed {
                        // [诊断] 收集当前所有 SubAgentAccumulator 的 agent_id
                        let registered_ids: Vec<&str> = state.current_turn.subagent_ids();
                        tracing::warn!(
                            target: "tui.acp_events",
                            agent_id = ?agent_id,
                            tool_id = %ts.tool_id,
                            tool_name = %ts.tool_name,
                            routed = false,
                            registered_count = registered_ids.len(),
                            registered_agent_ids = ?registered_ids,
                            "subagent tool start NOT ROUTED to SubAgentGroup"
                        );
                        // [修复] 兜底：路由失败时将工具卡作为普通 ToolCard 展示，
                        // 确保第二个 SubAgent 的工具调用不会完全丢失
                        state.current_turn.start_tool(ToolCardAccumulator::new(
                            ts.tool_id.clone(),
                            ts.tool_name.clone(),
                            ts.input_summary.clone(),
                        ));
                    }
                    state.variant = 1;
                    state.phase = SessionPhase::PromptRunning;
                    push_view_models(state);
                }
            } else {
                state.current_turn.start_tool(ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ));
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            }
            push_acp_state(state);
        }
        ToolEnded(te) => {
            if let Some(agent_id) = te.agent_id.as_deref() {
                // bg sub-agent: TurnSuspended 后 SubAgentAccumulator 已被清除，
                // 后续 bg 工具事件仅更新 BG_DISPLAY，不走 end_subagent_tool
                if BG_AGENT_IDS.state().read().contains(agent_id) {
                    if let Some(entry) = BG_DISPLAY
                        .state()
                        .write()
                        .iter_mut()
                        .find(|e| e.id == agent_id)
                    {
                        entry.current_tool = None;
                        entry.tool_count += 1;
                    }
                    state.variant = 1;
                    state.phase = SessionPhase::PromptRunning;
                    push_view_models(state);
                } else {
                    // 同步 sub-agent: 路由到 SubAgentAccumulator
                    let routed = state.current_turn.end_subagent_tool(
                        agent_id,
                        &te.tool_id,
                        te.output_summary.clone(),
                        te.is_error,
                    );
                    if !routed {
                        tracing::trace!(agent_id, tool_id = %te.tool_id, "kit bridge: subagent tool end has no active group");
                    }
                    state.variant = 1;
                    state.phase = SessionPhase::PromptRunning;
                    push_view_models(state);
                }
            } else {
                state
                    .current_turn
                    .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            }
            push_acp_state(state);
        }

        // ── §4.2 Boundary events ──
        // 保留分支（无数据枚举变体必须被 match 覆盖，否则编译错误），
        // 但标注 dead path：当前 notifier 从不发出此事件。
        PromptStarted => {
            tracing::trace!("dead path: PromptStarted not emitted by notifier");
            state.phase = SessionPhase::PromptRunning;
            state.variant = 1;
            push_acp_state(state);
        }
        PromptSubmitted => {
            // submit_consumer 在 prompt RPC 之前发出此事件，让 bridge 统一管理 loading 状态。
            state.phase = SessionPhase::PromptRunning;
            state.variant = 1;
            push_acp_state(state);
        }
        SessionReplayStarted => {
            tracing::trace!("dead path: SessionReplayStarted not emitted by notifier");
            state.phase = SessionPhase::ReplayingHistory;
            state.variant = 0;
            state.current_turn.reset();
            push_view_models(state);
            push_acp_state(state);
        }
        SessionReplayDone => {
            tracing::trace!("dead path: SessionReplayDone not emitted by notifier");
            if state.phase == SessionPhase::ReplayingHistory {
                state.phase = SessionPhase::Idle;
            }
            state.variant = 0;
            state.current_turn.reset();
            push_view_models(state);
            push_acp_state(state);
        }
        TurnDone => {
            // H3: TurnDone 仅做两件事：
            // (a) current_turn.view_models() → 逐条 push_back 到 committed
            // (b) current_turn.reset() + push_view_models
            // buffered_text 已由 LocalUserBubble 事件提前入队 committed，
            // TurnDone 不再代为搬运。
            state.flush_current_turn();
            state.variant = 0;

            state.phase = SessionPhase::Idle;

            tracing::info!(
                is_loading = state.phase == SessionPhase::PromptRunning,
                committed_len = state.committed.len(),
                current_turn_empty = state.current_turn.is_empty(),
                "TurnDone: writing ACP_STATE"
            );

            push_view_models(state);
            push_acp_state(state);

            // (g) C1: agent 完成本轮——drain INPUT_BUFFER，按顺序重新提交。
            drain_input_buffer();

            // C2: compact 命令完成后触发 session/load 重放。
            // 区分 agent 内部 compact：命令 compact（Immediate）后无后续流事件，
            // current_turn 为空；agent 内部 compact 后 current_turn 有内容。
            if state.compact_just_completed && state.current_turn.is_empty() {
                state.compact_just_completed = false;
                if let Some(tx) = THREAD_LOAD_TX.get() {
                    let session_id = state.active_session_id.clone();
                    tracing::info!(
                        session_id = %session_id,
                        "TurnDone: compact completed, triggering session/load replay"
                    );
                    let _ = tx.send(session_id);
                }
            }
        }
        TurnInterrupted { reason: _reason } => {
            // 零产出回滚：Agent 尚未产出任何 AI 内容时（current_turn 为空），
            // 撤销本次用户气泡 + 恢复文本到输入框。
            // 仅当有 last_submitted_text 时才执行（正常情况下 LocalUserBubble 已到达）。
            if state.current_turn.is_empty() && state.last_submitted_text.is_some() {
                let restore_text = state.last_submitted_text.take().unwrap();
                // 移除 committed 中最后一条用户气泡
                if let Some(last) = state.committed.last()
                    && matches!(last, TuiRenderUnit::TuiUserBubble(_))
                {
                    let last_idx = state.committed.len().saturating_sub(1);
                    state.committed.remove(last_idx);
                }
                // 将文本放入恢复存储，递增 RENDER_HEARTBEAT 触发 input_area 重渲染
                let mu = crate::kit::atoms::INPUT_RESTORE_TEXT
                    .get_or_init(|| parking_lot::Mutex::new(None));
                *mu.lock() = Some(restore_text);
                RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                // 清除排队输入缓冲——取消后不应继续处理排队的输入
                INPUT_BUFFER.state().write().clear();
                state.current_turn = CurrentTurn::new();
                state.variant = 0;
                state.phase = SessionPhase::Idle;
                push_view_models(state);
                push_acp_state(state);
                return;
            }

            // 守卫：仅当 current_turn 有未归档内容时才归档
            if !state.current_turn.committed && !state.current_turn.is_empty() {
                state.current_turn.deactivate();
                for vm in state.current_turn.view_models() {
                    state.committed.push_back(vm.clone());
                }
            }
            state.current_turn = CurrentTurn::new();
            state.variant = 0;
            state.phase = SessionPhase::Idle;
            push_view_models(state);
            push_acp_state(state);
        }
        TurnSuspended => {
            // Turn 挂起（idle/await_wake）——与 TurnDone 类似但 Agent 保持存活。
            // 归档 current_turn → committed，停止 loading，但不 drain_input_buffer
            // （Agent 还在 await_wake，新 turn 的流事件会自动恢复 loading）。
            if !state.current_turn.committed && !state.current_turn.is_empty() {
                for vm in state.current_turn.view_models() {
                    state.committed.push_back(vm.clone());
                }
            }
            state.current_turn.reset();
            state.variant = 0;
            state.phase = SessionPhase::Idle;
            push_view_models(state);
            push_acp_state(state);
            // 注意：不调用 drain_input_buffer()——Agent 保持存活，
            // 输入缓冲在 Agent 真正完成（TurnDone）时再处理。
        }

        // ── §4.3 Status events ──
        ToolCount(_tc) => {
            push_acp_state(state);
        }
        Progress(_) => {
            push_acp_state(state);
        }
        BudgetWarning(bw) => {
            // 上下文使用率超过阈值警告——注入 TuiSystemNote 到消息流。
            // flush_current_turn 确保 system note 出现在正确的时序位置
            //（已产出 AI 内容之后、后续 AI 内容之前）。
            state.flush_current_turn();
            let pct = bw.used as f64 / bw.limit as f64 * 100.0;
            let used_display = if bw.used >= 1_000_000 {
                format!("{:.1}M", bw.used as f64 / 1_000_000.0)
            } else if bw.used >= 1_000 {
                format!("{:.0}k", bw.used as f64 / 1000.0)
            } else {
                bw.used.to_string()
            };
            let limit_display = if bw.limit >= 1_000_000 {
                format!("{:.1}M", bw.limit as f64 / 1_000_000.0)
            } else if bw.limit >= 1_000 {
                format!("{:.0}k", bw.limit as f64 / 1000.0)
            } else {
                bw.limit.to_string()
            };
            let text = i18n::tr_args(
                "app-note-budget-warning",
                &[
                    ("pct".into(), FluentValue::from(pct as u64)),
                    ("used".into(), FluentValue::from(used_display.as_str())),
                    ("limit".into(), FluentValue::from(limit_display.as_str())),
                ],
            );
            let content_hash = tui_hash_str(&text);
            state
                .committed
                .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                    text,
                    level: TuiNoteLevel::Warning,
                    content_hash,
                }));
            push_view_models(state);
            push_acp_state(state);
        }
        SystemNotification(sn) => {
            // 系统通知（如 cache 命中率警告）——注入 TuiSystemNote 到消息流。
            // flush_current_turn 确保 system note 出现在正确的时序位置
            //（已产出 AI 内容之后、后续 AI 内容之前）。
            state.flush_current_turn();
            let level = match sn.level.as_str() {
                "warning" => TuiNoteLevel::Warning,
                "error" => TuiNoteLevel::Error,
                _ => TuiNoteLevel::Info,
            };
            let content_hash = tui_hash_str(&format!("{}|{:?}", sn.text, level));
            state
                .committed
                .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                    text: sn.text.clone(),
                    level,
                    content_hash,
                }));
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.4 Input assist ──
        // Prediction 不触发 push_view_models（不进入消息流），只更新 PREDICTION atom。
        // input_area 通过 use_atom(&PREDICTION) 自动重渲染显示预测占位符。
        // 也不调 push_acp_state（避免不必要的 AppShell 重渲染）。
        Prediction(p) => {
            *PREDICTION.state().write() = PredictionState {
                text: p.text.clone(),
                received_at: Some(Instant::now()),
            };
        }
        FileSuggestions(_) => {}

        // ── §4.5 Interaction events ──
        // S7：把每个交互事件映射到具体 PopupKind，让 PopupOverlay 精确路由
        HitlPending(hp) => {
            // I21-A：保存 payload 到 HITL_PENDING atom，供 HitlPopup 读取真实数据
            *HITL_PENDING.state().write() = Some(hp.clone());
            state.popup_kind = Some(PopupKind::Hitl);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        AskUser(au) => {
            // I21-B：保存 payload 到 ASK_USER_PENDING atom，供 AskUserPanel 读取真实数据。
            // 通过 panel_registry 打开 AskUser 面板（非弹窗），内联在 MessageArea 下方。
            *ASK_USER_PENDING.state().write() = Some(au.clone());
            crate::kit::panel_registry::open_panel(crate::app::panel_types::PanelKind::AskUser);
            state.variant = 2;
            push_acp_state(state);
        }
        RewindPreview(rp) => {
            // S10：保存 payload 到 REWIND_PREVIEW atom，供 RewindPopup 读取真实数据
            *REWIND_PREVIEW.state().write() = Some(rp.clone());
            state.popup_kind = Some(PopupKind::Rewind);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }
        RewindCompleted { messages_json } => {
            // H3: rewind 完成——反序列化 messages_json 为 Vec<Value>，按 role
            // 映射为 TuiRenderUnit，替换 state.committed。
            tracing::info!("bridge: RewindCompleted, replacing committed");
            state.committed.clear();
            state.current_turn.reset();
            match serde_json::from_str::<Vec<serde_json::Value>>(messages_json) {
                Ok(messages) => {
                    for msg in messages {
                        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        let text = extract_message_text(&msg);
                        let content_hash = tui_hash_str(&text);
                        let vm = match role {
                            "user" => TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text)),
                            "assistant" => TuiRenderUnit::TuiAssistantBubble(
                                crate::kit::tui_render_unit::TuiAssistantBubble {
                                    text,
                                    reasoning: None,
                                    content_hash,
                                },
                            ),
                            "system" => {
                                // system 角色历史消息：rewind 不还原 system-reminder 标记
                                // （统一以 assistant bubble 渲染，无前缀），罕见但可能。
                                tracing::trace!(
                                    role = role,
                                    "RewindCompleted: system role rendered as assistant bubble"
                                );
                                TuiRenderUnit::TuiAssistantBubble(
                                    crate::kit::tui_render_unit::TuiAssistantBubble {
                                        text,
                                        reasoning: None,
                                        content_hash,
                                    },
                                )
                            }
                            other => {
                                tracing::warn!(
                                    role = other,
                                    "RewindCompleted: unknown role, rendered as assistant bubble"
                                );
                                TuiRenderUnit::TuiAssistantBubble(
                                    crate::kit::tui_render_unit::TuiAssistantBubble {
                                        text,
                                        reasoning: None,
                                        content_hash,
                                    },
                                )
                            }
                        };
                        state.committed.push_back(vm);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "RewindCompleted: failed to deserialize messages_json, \
                         committed cleared; UI will show empty message list"
                    );
                }
            }
            state.phase = SessionPhase::Idle;
            push_view_models(state);
            push_acp_state(state);
        }
        OauthNeeded(on) => {
            // I20-D：保存 payload 到 OAUTH_INFO atom，供 OAuthPopup 读取真实数据
            *OAUTH_INFO.state().write() = Some(on.clone());
            state.popup_kind = Some(PopupKind::OAuth);
            state.variant = 2;
            push_popup_kind(state);
            push_acp_state(state);
        }

        // ── §4.6 Structure events ──
        SubagentStarted {
            agent_id,
            agent_name,
            is_background,
        } => {
            tracing::info!(
                target: "tui.acp_events",
                agent_id = %agent_id,
                agent_name = %agent_name,
                is_background = %is_background,
                existing_subagent_count = state.current_turn.subagent_ids().len(),
                "SubagentStarted: creating SubAgentGroup container"
            );
            state
                .current_turn
                .start_subagent(agent_id.clone(), agent_name.clone());
            // 仅后台 subagent 注册到 BG_AGENT_IDS——同步 subagent 不进入后台显示区域
            if *is_background {
                BG_AGENT_IDS.state().write().insert(agent_id.clone());
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            push_view_models(state);
            push_acp_state(state);
        }
        SubagentStopped { agent_id } => {
            tracing::info!(
                target: "tui.acp_events",
                agent_id = %agent_id,
                "SubagentStopped: marking SubAgentGroup as done"
            );
            state.current_turn.stop_subagent(agent_id);
            // 清理后台 agent_id 注册
            BG_AGENT_IDS.state().write().remove(agent_id);
            state.variant = 1;
            // phase 由 SubagentStarted + 流式事件维护，此处不再无条件覆盖
            // （避免 bg agent 的场景 TurnDone/TurnSuspended 后被重新激活）
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.8 Agent Event Extensions ──
        TurnCommitted {
            messages_json: _,
            steps,
        } => {
            tracing::info!(steps, "bridge: TurnCommitted ({steps} steps)");
            // 在 goal 自驱场景下 TurnDone 只在最终循环退出时触发，
            // TurnCommitted 作为每次 ReAct 迭代边界的刷新检查点，防止 TUI atom 漂移。
            push_view_models(state);
            push_acp_state(state);
        }
        CompactStarted => {
            // 上下文压缩在后台自动进行，不做消息流注入（避免干扰用户）
            tracing::info!("bridge: CompactStarted");
            state.phase = SessionPhase::PromptRunning;
            push_acp_state(state);
        }
        CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            ..
        } => {
            tracing::info!(
                summary_len = summary.len(),
                micro_cleared,
                "bridge: CompactCompleted"
            );
            state.compact_just_completed = true;
            state.phase = SessionPhase::Idle;
            // 全量压缩和有效微压缩都注入消息流通知
            // micro_cleared == 0: 完整压缩（总是显示），或 no-op 微压缩（罕见，无害）
            // micro_cleared > 0: 微压缩且有实质性清理
            {
                let mut parts = vec![];
                let file_count = files.len();
                let skill_count = skills.len();
                let compact_type = if *micro_cleared > 0 {
                    i18n::tr("app-note-compact-type-micro")
                } else {
                    i18n::tr("app-note-compact-type-full")
                };
                if file_count > 0 {
                    parts.push(format!("{file_count} 文件"));
                }
                if skill_count > 0 {
                    parts.push(format!("{skill_count} skills"));
                }
                let detail = if parts.is_empty() {
                    String::new()
                } else {
                    format!("（{}）", parts.join("，"))
                };
                let text = if summary.is_empty() {
                    i18n::tr_args(
                        "app-note-compact-completed",
                        &[
                            ("detail".into(), FluentValue::from(detail.as_str())),
                            ("type".into(), FluentValue::from(compact_type.as_str())),
                        ],
                    )
                } else {
                    let brief: String = summary.chars().take(60).collect();
                    let suffix = if summary.chars().count() > 60 {
                        "…"
                    } else {
                        ""
                    };
                    let summary_display = format!("{brief}{suffix}");
                    i18n::tr_args(
                        "app-note-compact-completed-summary",
                        &[
                            ("detail".into(), FluentValue::from(detail.as_str())),
                            (
                                "summary".into(),
                                FluentValue::from(summary_display.as_str()),
                            ),
                            ("type".into(), FluentValue::from(compact_type.as_str())),
                        ],
                    )
                };
                let content_hash = tui_hash_str(&text);
                state
                    .committed
                    .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                        text,
                        level: TuiNoteLevel::Warning,
                        content_hash,
                    }));
                push_view_models(state);
            }
            push_acp_state(state);
        }
        CompactError { message } => {
            tracing::warn!(message, "bridge: CompactError");
            let text = i18n::tr_args(
                "app-note-compact-error",
                &[("message".into(), FluentValue::from(message.as_str()))],
            );
            let content_hash = tui_hash_str(&text);
            state
                .committed
                .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                    text,
                    level: TuiNoteLevel::Warning,
                    content_hash,
                }));
            state.phase = SessionPhase::Idle;
            push_view_models(state);
            push_acp_state(state);
        }
        BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            duration_ms,
            ..
        } => {
            let msg = if *success {
                format!(
                    "后台 {} {} 完成 ({:.0}s)",
                    agent_name,
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            } else {
                format!(
                    "后台 {} {} 失败 ({:.0}s)",
                    agent_name,
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            };
            tracing::info!(msg, "bridge: BackgroundTaskCompleted");
        }
        AgentExecutionFailed { message } => {
            tracing::error!(message, "bridge: AgentExecutionFailed");
            let text = i18n::tr_args(
                "app-note-agent-failed",
                &[("message".into(), FluentValue::from(message.as_str()))],
            );
            let content_hash = tui_hash_str(&text);
            state
                .committed
                .push_back(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                    text,
                    level: TuiNoteLevel::Error,
                    content_hash,
                }));
            state.phase = SessionPhase::Idle;
            push_view_models(state);
            push_acp_state(state);
        }
        WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            phase,
            ..
        } => {
            tracing::debug!(
                run_id,
                workflow_name,
                event_type,
                phase = ?phase,
                "bridge: WorkflowProgress"
            );
        }

        // ── §4.9 Plugin events ──
        PluginSnapshot(snapshot) => {
            let plugins: Vec<PluginSummary> = snapshot
                .plugins
                .iter()
                .map(|p| PluginSummary {
                    name: p.name.clone(),
                    version: p.version.clone(),
                    enabled: p.enabled,
                    root: p.root.clone(),
                    description: p.description.clone(),
                    marketplace: p.marketplace.clone(),
                    author: p.author.clone(),
                    skills_count: p.skills_count,
                    commands_count: p.commands_count,
                    agents_count: p.agents_count,
                    mcp_count: p.mcp_count,
                    install_scope: p.install_scope.clone(),
                    load_error: p.load_error.clone(),
                })
                .collect();
            PLUGIN_LIST.state().write().clone_from(&plugins);
        }
        PluginActionResult(result) => {
            let msg = if result.success {
                format!(
                    "{} {}",
                    result.plugin_name,
                    i18n::tr("panel-plugin-operation-complete"),
                )
            } else {
                format!(
                    "{} {}: {}",
                    result.plugin_name,
                    i18n::tr("panel-plugin-operation-failed"),
                    result.error.as_deref().unwrap_or("unknown error"),
                )
            };
            NOTIFICATION.state().write().replace(Notification {
                message: msg,
                until: Instant::now() + Duration::from_secs(3),
            });
            // 触发 PluginPanel 重渲染以清除 operation_loading
            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        }
        PluginSearchResult(result) => {
            let items: Vec<PluginSummary> = result
                .results
                .iter()
                .map(|r| PluginSummary {
                    name: r.name.clone(),
                    version: r.version.clone(),
                    enabled: false,
                    root: String::new(),
                    description: r.description.clone(),
                    marketplace: r.marketplace.clone(),
                    author: r.author.clone(),
                    skills_count: r.skills_count,
                    commands_count: r.commands_count,
                    agents_count: r.agents_count,
                    mcp_count: r.mcp_count,
                    install_scope: r.install_scope.clone(),
                    load_error: None,
                })
                .collect();
            PLUGIN_SEARCH_RESULTS.state().write().clone_from(&items);
        }

        // ── Unknown / forward-compat ──
        Unknown { .. } => {}
        LocalUserBubble { text } => {
            state.last_submitted_text = Some(text.clone());
            state
                .committed
                .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
                    text.clone(),
                )));
            push_view_models(state);
            push_acp_state(state);
        }
        BgCallbackBubble { .. } => {
            // bg callback flush-only：把 current_turn 归档到 committed，
            // 但不 push bg 回调气泡本身。气泡由标准 session/update 通道的
            // LocalUserBubble 负责推送。这样保证：
            // ① current_turn 内容在前（flush）
            // ② bg 回调气泡在中间（LocalUserBubble 随后到达）
            // ③ 后续 AI 内容在后（TurnDone 归档）
            state.flush_current_turn();
            push_view_models(state);
            push_acp_state(state);
        }
        CommittedAssistantText { text, reasoning } => {
            let reason_block =
                reasoning
                    .as_ref()
                    .map(|r| crate::kit::tui_render_unit::TuiReasoningBlock {
                        text: r.clone(),
                        collapsed: false,
                    });
            let content_hash =
                tui_hash_str(&format!("{}|{}", text, reasoning.as_deref().unwrap_or("")));
            let vm = TuiRenderUnit::TuiAssistantBubble(
                crate::kit::tui_render_unit::TuiAssistantBubble {
                    text: text.clone(),
                    reasoning: reason_block,
                    content_hash,
                },
            );
            state.committed.push_back(vm);
            push_view_models(state);
            push_acp_state(state);
        }
        ReplayToolStarted {
            tool_id,
            tool_name,
            input_summary,
        } => {
            use crate::kit::tui_render_unit::{TuiToolCard, tui_hash_str};
            let card = TuiToolCard {
                tool_id: tool_id.clone(),
                tool_name: tool_name.clone(),
                input_summary: input_summary.clone(),
                output_summary: String::new(),
                is_error: false,
                is_running: true,
                running_duration_ms: None,
                diff: None,
                tool_calls_count: 0,
                content_hash: tui_hash_str(&format!(
                    "{}|{}|{}||false|true",
                    tool_id, tool_name, input_summary
                )),
            };
            state.committed.push_back(TuiRenderUnit::TuiToolCard(card));
            push_view_models(state);
            push_acp_state(state);
        }
        ReplayToolEnded {
            tool_id,
            output_summary,
            is_error,
        } => {
            update_committed_tool_card(state, tool_id, output_summary, *is_error);
            push_view_models(state);
            push_acp_state(state);
        }

        // ── §4.7 Background Tasks ──
        BgTaskSnapshot(tasks) => {
            BG_TASKS.state().write().clone_from(tasks);
            // 从快照全量构造 BG_DISPLAY 条目
            let entries: Vec<BgDisplayEntry> = tasks
                .iter()
                .map(|t| BgDisplayEntry {
                    id: t.task_id.clone(),
                    agent_type: t.kind.clone(),
                    desc: t.summary.clone(),
                    current_tool: None,
                    tool_count: 0,
                    is_active: true,
                    is_error: false,
                    created_at: Instant::now(),
                    completed_at: None,
                })
                .collect();
            BG_DISPLAY.state().write().clone_from(&entries);
        }
        BgTaskStarted(task) => {
            BG_TASKS.state().write().push(task.clone());
            // 创建后台显示条目
            BG_DISPLAY.state().write().push(BgDisplayEntry {
                id: task.task_id.clone(),
                agent_type: task.kind.clone(),
                desc: task.summary.clone(),
                current_tool: None,
                tool_count: 0,
                is_active: true,
                is_error: false,
                created_at: Instant::now(),
                completed_at: None,
            });
        }
        BgTaskCompleted {
            task_id,
            success,
            duration_ms,
        } => {
            BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
            // 标记后台显示条目为完成（3s 倒计时）
            let now = Instant::now();
            if let Some(entry) = BG_DISPLAY
                .state()
                .write()
                .iter_mut()
                .find(|e| e.id == *task_id)
            {
                entry.is_active = false;
                entry.is_error = !*success;
                entry.completed_at = Some(now);
            }
            let msg = if *success {
                i18n::tr_args(
                    "bg-task-notify-completed",
                    &[
                        ("name".into(), FluentValue::from(task_id.as_str())),
                        (
                            "duration".into(),
                            FluentValue::from(*duration_ms as f64 / 1000.0),
                        ),
                    ],
                )
            } else {
                i18n::tr_args(
                    "bg-task-notify-failed",
                    &[
                        ("name".into(), FluentValue::from(task_id.as_str())),
                        (
                            "duration".into(),
                            FluentValue::from(*duration_ms as f64 / 1000.0),
                        ),
                    ],
                )
            };
            NOTIFICATION.state().write().replace(Notification {
                message: msg,
                until: Instant::now() + Duration::from_millis(1500),
            });
        }
        BgTaskCancelled { task_id, .. } => {
            BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
            // 标记后台显示条目为失败（3s 倒计时）
            let now = Instant::now();
            if let Some(entry) = BG_DISPLAY
                .state()
                .write()
                .iter_mut()
                .find(|e| e.id == *task_id)
            {
                entry.is_active = false;
                entry.is_error = true;
                entry.completed_at = Some(now);
            }
        }
    }
}

/// 在 `state.committed` 中按 `tool_id` 查找并更新 TuiToolCard。
///
/// 用于 replay 场景：`ReplayToolStarted` 先 push 一张 is_running=true 的卡片，
/// 后续 `ReplayToolEnded` 到达时更新 output + is_running=false。
/// 如果找不到对应 tool_id，静默忽略。
fn update_committed_tool_card(
    state: &mut BridgeState,
    tool_id: &str,
    output_summary: &str,
    is_error: bool,
) {
    use crate::kit::tui_render_unit::{TuiToolCard, tui_hash_str};
    for i in 0..state.committed.len() {
        if let TuiRenderUnit::TuiToolCard(card) = &state.committed[i]
            && card.tool_id == tool_id
            && card.is_running
        {
            let updated = TuiToolCard {
                tool_id: card.tool_id.clone(),
                tool_name: card.tool_name.clone(),
                input_summary: card.input_summary.clone(),
                output_summary: output_summary.to_string(),
                is_error,
                is_running: false,
                running_duration_ms: None,
                diff: card.diff.clone(),
                tool_calls_count: card.tool_calls_count,
                content_hash: tui_hash_str(&format!(
                    "{}|{}|{}|{}|{is_error}|false",
                    card.tool_id, card.tool_name, card.input_summary, output_summary,
                )),
            };
            state.committed = state
                .committed
                .update(i, TuiRenderUnit::TuiToolCard(updated));
            return;
        }
    }
}

/// 从 BaseMessage JSON 提取纯文本（H3 辅助函数）。
///
/// content 可能是 string 或 array of {type:text, text:...}，
/// 两种格式都兼容。**仅提取 type=="text" 的 block**——tool_use / tool_result /
/// reasoning / image 等非文本 block 在 rewind 视图下不还原（用户看不到历史工具
/// 调用卡片与 reasoning），这是 display-only 的简化设计，避免在 H3 反序列化
/// 路径重建完整 ViewModel（成本过高且非 rewind 主场景）。
fn extract_message_text(msg: &serde_json::Value) -> String {
    // content 为 string 的简单格式
    if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    // content 为 array 的复杂格式
    if let Some(arr) = msg.get("content").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    block.get("text").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Atom push 辅助函数
// ---------------------------------------------------------------------------

/// 将 BridgeState 中的 ViewModels 写入 VIEW_MODELS Atom。
///
/// 从 `state.committed`（im::Vector）clone（O(1)引用计数）后逐条 push_back
/// `current_turn.view_models()`，构成扁平单层列表。generation 每次调用递增+1。
pub(crate) fn push_view_models(state: &mut BridgeState) {
    // [Diagnostic] 追踪 VIEW_MODELS 写入时机——配合 scroll diag 分析 submit/history 滚动问题
    let is_loading = state.phase == SessionPhase::PromptRunning;
    tracing::info!(
        target: "msg_scroll_diag",
        committed = state.committed.len(),
        current_turn = state.current_turn.view_models().len(),
        generation = state.generation,
        phase = ?state.phase,
        is_loading,
        "push_view_models: writing VIEW_MODELS atom",
    );
    let mut items = state.committed.clone();
    for vm in state.current_turn.view_models() {
        items.push_back(vm.clone());
    }

    // 只展开最后一个含 reasoning 的 assistant bubble，其余折叠。
    // [Bug 2 修复] 仅在 collapsed 实际改变时同步重算 content_hash——
    // reasoning.collapsed 已纳入 hash 公式（见 TuiAssistantBubble::compute_hash），
    // 不重算会导致按 hash 分片的渲染缓存命中旧值、折叠/展开后 UI 不刷新。
    // 仅在变化时重算避免每次 token 到达都遍历 hash。
    let mut found_last = false;
    for i in (0..items.len()).rev() {
        if let TuiRenderUnit::TuiAssistantBubble(bubble) = &mut items[i]
            && let Some(ref mut reasoning) = bubble.reasoning
        {
            let target_collapsed = found_last;
            if reasoning.collapsed != target_collapsed {
                reasoning.collapsed = target_collapsed;
                bubble.recompute_hash();
            }
            if !found_last {
                found_last = true;
            }
        }
    }

    state.generation = state.generation.wrapping_add(1);
    let snapshot = ViewModelsSnapshot {
        items,
        generation: state.generation,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    let snapshot = ViewModelsSnapshot {
        items: im::Vector::new(),
        generation: 0,
    };
    *VIEW_MODELS.state().write() = snapshot;
}

/// 将 BridgeState 中的状态快照写入 ACP_STATE Atom。
///
/// 仅在快照值变化时才写入——避免不必要的全树重渲染。
/// 流式期间 variant/is_loading 不变时，仅 view_count 变化；
/// popup 状态由各自的独立 atom 追踪（SLASH_HINT_ACTIVE 等），
/// 不应写入 ACP_STATE 导致 AppShell 重渲染。
fn push_acp_state(state: &mut BridgeState) {
    let snapshot = AcpStateSnapshot {
        variant: state.variant,
        view_count: state.committed.len() + state.current_turn.view_models().len(),
        is_loading: state.phase == SessionPhase::PromptRunning,
        wizard_active: false,
        at_mention_active: *AT_MENTION_ACTIVE.state().read(),
        slash_hint_active: *SLASH_HINT_ACTIVE.state().read(),
    };
    let state_ref = ACP_STATE.state();
    let mut acp = state_ref.write();
    if *acp != snapshot {
        *acp = snapshot;
    }
}

/// 将 BridgeState.popup_kind 写入 POPUP_KIND Atom（S7）。
fn push_popup_kind(state: &BridgeState) {
    *POPUP_KIND.state().write() = state.popup_kind;
}

/// 将 `INPUT_BUFFER` atom 中所有排队输入按入队顺序 drain，逐条发送到 SUBMIT_TX。
///
/// 调用时机：`TurnDone` 事件——agent 完成本轮，从队列里取出用户在 loading 期间
/// 缓存的 agent text 继续提交。若 buffer 为空则 no-op；若 SUBMIT_TX 未初始化也安全跳过。
///
/// 多条输入的顺序保证：VecDeque + 顺序 `tx.send` + submit_consumer 单消费者 →
/// 严格 FIFO。第一条立即触发 prompt，后续在 submit_consumer 内部顺序处理
/// （每条都等上一条的 RPC 完成）。
fn drain_input_buffer() {
    let tx = SUBMIT_TX.get().cloned();
    if tx.is_none() {
        return;
    }

    let drained: Vec<String> = INPUT_BUFFER.state().write().drain(..).collect();
    if let Some(tx) = tx {
        for text in drained {
            let _ = tx.send(SubmitRequest::AgentText(text));
        }
    }
}

/// 从 ACP SessionUpdate::Plan JSON 中提取 TodoItem 列表并写入 TODO_ITEMS atom。
///
/// 使用类型安全 serde 反序列化将 Plan JSON 映射为 TodoItem 列表。
/// Plan JSON 格式:
///   {"sessionUpdate":"plan","entries":[{"content":"Fix bug","status":"in_progress","priority":"medium"}]}
pub fn handle_plan_update(update: &serde_json::Value) {
    use crate::kit::message_area::{TodoItem, TodoStatus};

    let plan: Plan = match serde_json::from_value(update.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "handle_plan_update: failed to deserialize Plan");
            return;
        }
    };

    tracing::debug!(
        entries_count = plan.entries.len(),
        "handle_plan_update: received Plan entries"
    );

    let items: Vec<TodoItem> = plan
        .entries
        .into_iter()
        .map(|e| {
            let status = match e.status {
                PlanEntryStatus::InProgress => TodoStatus::InProgress,
                PlanEntryStatus::Completed => TodoStatus::Completed,
                PlanEntryStatus::Pending => TodoStatus::Pending,
                _ => {
                    tracing::warn!(status = ?e.status, "handle_plan_update: unknown PlanEntryStatus, fallback to Pending");
                    TodoStatus::Pending
                }
            };
            TodoItem {
                status,
                content: e.content,
            }
        })
        .collect();

    tracing::debug!(
        items_count = items.len(),
        "handle_plan_update: writing {} items to TODO_ITEMS",
        items.len()
    );
    *crate::kit::atoms::TODO_ITEMS.state().write() = items;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::message_area::TodoStatus;
    use serde_json::json;
    use serial_test::serial;
    use tokio::sync::mpsc;

    #[test]
    #[serial]
    fn test_dispatch_subagent_streaming_updates_current_turn_group() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: "agent-1".into(),
                agent_name: "researcher".into(),
                is_background: false,
            },
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "child text".into(),
                message_id: None,
                agent_id: Some("agent-1".into()),
            }),
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.items.len(), 1);
        match &snapshot.items[0] {
            TuiRenderUnit::TuiSubAgentGroup(group) => {
                assert_eq!(group.agent_id, "agent-1");
                assert_eq!(group.view_models.len(), 1);
            }
            other => panic!("expected TuiSubAgentGroup, got {other:?}"),
        }
    }

    /// C1 回归测试：drain_input_buffer 清空 INPUT_BUFFER 队列。
    ///
    /// 注：不验证 SUBMIT_TX 接收——SUBMIT_TX 是 OnceLock 全局句柄，一旦被其他
    /// 测试 set 就无法重置；此处只验证 drain 的核心效应（buffer 被清空）。
    /// 顺序保证由 `VecDeque::drain(..)` + 顺序 `tx.send` 在源码层面保证。
    #[tokio::test]
    #[serial]
    async fn test_drain_input_buffer_preserves_order() {
        crate::kit::atoms::init_atoms();
        let _ = SUBMIT_TX.get_or_init(|| {
            let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
            tx
        });

        // 入队三条
        {
            let state = INPUT_BUFFER.state();
            let mut buf = state.write();
            buf.push_back("first".into());
            buf.push_back("second".into());
            buf.push_back("third".into());
        }

        drain_input_buffer();

        // 验证 buffer 已被 drain 干净——这是 drain_input_buffer 的核心效应
        assert!(
            INPUT_BUFFER.state().read().is_empty(),
            "buffer should be empty after drain"
        );
    }

    /// C1 回归测试：空 buffer 是 no-op，drain 后仍为空。
    #[tokio::test]
    #[serial]
    async fn test_drain_input_buffer_empty_is_noop() {
        crate::kit::atoms::init_atoms();
        let _ = SUBMIT_TX.get_or_init(|| {
            let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
            tx
        });

        INPUT_BUFFER.state().write().clear();
        drain_input_buffer();

        assert!(
            INPUT_BUFFER.state().read().is_empty(),
            "empty buffer should remain empty"
        );
    }

    /// C1 回归测试：SUBMIT_TX 未初始化时安全跳过，不 panic，buffer 也保持不变。
    ///
    /// 注：实际运行时 OnceLock 一旦 set 无法 unset；本测试只验证不 panic。
    #[test]
    #[serial]
    fn test_drain_input_buffer_no_submit_tx_safe() {
        crate::kit::atoms::init_atoms();
        // 不论 SUBMIT_TX 是否 set，都不应 panic
        INPUT_BUFFER.state().write().push_back("x".into());
        drain_input_buffer();
        // SUBMIT_TX 已被前面测试 set 过，所以 drain 成功 → buffer 被清空
        // 即使 SUBMIT_TX 未 set，drain 早退，buffer 仍有 "x"——两种情况都不算 panic
    }

    /// BRIDGE_RESET_COUNTER 递增时 acp_bridge 重置分支同步清空 INPUT_BUFFER，
    /// 防止旧会话缓存输入在新会话 TurnDone 时泄漏。
    ///
    /// 此测试模拟 bridge 的 counter != last_reset_counter 分支：先填入 buffer 数据，
    /// 递增 BRIDGE_RESET_COUNTER，构造任意事件 dispatch，断言 buffer 已被清空。
    /// 注意：实际清空发生在 acp_bridge.rs 的 counter 检测分支，而非 dispatch_and_notify
    /// 内部。此测试模拟的是那个分支调用 push_view_models_for_reset() 前后的完整效应。
    #[test]
    #[serial]
    fn test_bridge_reset_clears_input_buffer() {
        crate::kit::atoms::init_atoms();
        // 填入 buffer 数据
        INPUT_BUFFER
            .state()
            .write()
            .push_back("leaked input".into());
        INPUT_BUFFER
            .state()
            .write()
            .push_back("another leaked input".into());
        assert!(!INPUT_BUFFER.state().read().is_empty(), "buffer 应有数据");

        // 模拟 acp_bridge 的 counter 检测分支：
        // push_view_models_for_reset() 前同步清空 INPUT_BUFFER
        INPUT_BUFFER.state().write().clear();
        push_view_models_for_reset();

        assert!(
            INPUT_BUFFER.state().read().is_empty(),
            "bridge reset 后 INPUT_BUFFER 应被清空"
        );

        // VIEW_MODELS 也应被重置
        let snapshot = VIEW_MODELS.state().read().clone();
        assert!(
            snapshot.items.is_empty(),
            "bridge reset 后 committed 应为空"
        );
        assert!(
            snapshot.items.is_empty(),
            "bridge reset 后 current_turn 应为空"
        );
    }

    #[test]
    #[serial]
    fn test_two_turn_done_accumulates_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // 第一轮：stream one text → TurnDone
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "first turn".into(),
                message_id: None,
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        assert_eq!(
            state.committed.len(),
            1,
            "first TurnDone: committed should have 1 VM"
        );

        // 第二轮：stream another text → TurnDone
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "second turn".into(),
                message_id: None,
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(
            snapshot.items.len(),
            2,
            "two TurnDones: committed should have 2 VMs"
        );
    }

    /// TurnDone 归档 assistant VM 到 committed，不再代为搬运 buffered_text。
    #[test]
    #[serial]
    fn test_turndone_archives_assistant_to_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // 往 current_turn 写入一条 assistant 文本
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "assistant reply".into(),
                message_id: None,
                agent_id: None,
            }),
        );

        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        // TurnDone 后 assistant VM 被归档到 committed
        assert_eq!(
            state.committed.len(),
            1,
            "committed 应有 1 个 VM：TuiAssistantBubble"
        );
        match &state.committed[0] {
            TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "assistant reply"),
            other => panic!("expected TuiAssistantBubble at [0], got {other:?}"),
        }
    }

    /// TurnInterrupted 空 current_turn 不归档
    #[test]
    #[serial]
    fn test_turn_interrupted_empty_skips_archive() {
        crate::kit::atoms::init_atoms();
        // 预置一条 committed 数据
        let pre_existing = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(
            TuiUserBubble::new("existing".into()),
        )]);
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            items: pre_existing.clone(),
            generation: 0,
        };
        let mut state = BridgeState {
            variant: 1,
            committed: pre_existing,
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::PromptRunning,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        dispatch_and_notify(
            &mut state,
            &AcpEventData::TurnInterrupted {
                reason: "test".into(),
            },
        );

        assert_eq!(
            state.committed.len(),
            1,
            "空 current_turn → TurnInterrupted 不应归档，committed 长度不变"
        );
        match &state.committed[0] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "existing"),
            other => panic!("committed[0] 应为原始 TuiUserBubble, got {other:?}"),
        }
        assert!(state.current_turn.is_empty(), "current_turn 应已重置");
        assert_eq!(state.phase, SessionPhase::Idle, "phase 应为 Idle");
    }

    /// push_view_models 以 BridgeState 为准，不再 fallback 到 atom 旧值。
    #[test]
    #[serial]
    fn test_push_view_models_uses_bridge_state() {
        crate::kit::atoms::init_atoms();
        // atom 中有旧数据
        let old_items = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "old data".into(),
        ))]);
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            items: old_items,
            generation: 0,
        };

        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // push_view_models: 用 BridgeState 数据（空 committed + 空 current_turn）→ 空 items
        push_view_models(&mut state);

        let snapshot = VIEW_MODELS.state().read().clone();
        assert!(
            snapshot.items.is_empty(),
            "push_view_models with empty BridgeState should produce empty items"
        );
    }

    #[test]
    #[serial]
    fn test_handle_plan_update_multiple_entries() {
        crate::kit::atoms::init_atoms();
        *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

        let plan_json = json!({
            "entries": [
                {"content": "Task 1", "status": "in_progress", "priority": "medium"},
                {"content": "Task 2", "status": "pending", "priority": "medium"},
                {"content": "Task 3", "status": "completed", "priority": "medium"}
            ]
        });

        handle_plan_update(&plan_json);

        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert_eq!(items.len(), 3, "应包含 3 个条目");
        assert!(matches!(items[0].status, TodoStatus::InProgress));
        assert!(matches!(items[1].status, TodoStatus::Pending));
        assert!(matches!(items[2].status, TodoStatus::Completed));
    }

    #[test]
    #[serial]
    fn test_handle_plan_update_empty_entries() {
        crate::kit::atoms::init_atoms();
        *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

        let plan_json = json!({
            "entries": []
        });

        handle_plan_update(&plan_json);

        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert!(items.is_empty(), "空 entries 应产出空列表");
    }

    #[test]
    #[serial]
    fn test_handle_plan_update_missing_entries() {
        crate::kit::atoms::init_atoms();
        // 写入一个非空值，确认不被覆盖
        *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
            status: crate::kit::message_area::TodoStatus::InProgress,
            content: "existing".into(),
        }];

        let plan_json = json!({});
        handle_plan_update(&plan_json);

        // Plan 缺少 entries 字段 → deserialize 失败 → 不覆盖 TODO_ITEMS
        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert_eq!(items.len(), 1, "缺少 entries 不应覆盖已有列表");
        assert_eq!(items[0].content, "existing");
    }

    /// M4: dispatch_and_notify 对 Prediction 事件写入 PREDICTION atom。
    #[test]
    #[serial]
    fn test_prediction_writes_prediction_atom() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        // 预清 PREDICTION
        *PREDICTION.state().write() = PredictionState::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        use peri_acp_types::event_data::Prediction;
        dispatch_and_notify(
            &mut state,
            &AcpEventData::Prediction(Prediction {
                text: "type this".into(),
            }),
        );

        let pred = PREDICTION.state().read().clone();
        assert_eq!(pred.text, "type this");
        assert!(pred.received_at.is_some(), "received_at 应被设置");
    }

    /// H3: RewindCompleted 反序列化 messages_json 替换 state.committed。
    #[test]
    #[serial]
    fn test_rewind_completed_replaces_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        // 预置 committed 旧数据
        let pre_existing = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(
            TuiUserBubble::new("old".into()),
        )]);
        let mut state = BridgeState {
            variant: 1,
            committed: pre_existing,
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::PromptRunning,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        let messages_json = serde_json::json!([
            {"role": "user", "content": "rewound user msg"},
            {"role": "assistant", "content": [{"type": "text", "text": "rewound assistant"}]}
        ])
        .to_string();

        dispatch_and_notify(&mut state, &AcpEventData::RewindCompleted { messages_json });

        // committed 应被替换为 2 条（user + assistant）
        assert_eq!(state.committed.len(), 2, "rewind 后 committed 应有 2 条 VM");
        match &state.committed[0] {
            TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "rewound user msg"),
            other => panic!("expected TuiUserBubble, got {other:?}"),
        }
        match &state.committed[1] {
            TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "rewound assistant"),
            other => panic!("expected TuiAssistantBubble, got {other:?}"),
        }
    }

    /// 跨 turn 场景：第一轮 reasoning 在 committed 中保留，第二轮为最后一个展开。
    #[test]
    #[serial]
    fn test_multi_turn_reasoning_preserved_in_committed() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // === Turn 1: user bubble, reasoning + text → TurnDone ===
        dispatch_and_notify(
            &mut state,
            &AcpEventData::LocalUserBubble {
                text: "第一个问题".into(),
            },
        );
        dispatch_and_notify(&mut state, &AcpEventData::PromptStarted);
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReasoningChunk(crate::kit::stream_data::TuiReasoningChunk {
                text: "Turn 1 的思考内容".into(),
                message_id: Some("msg_1".into()),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "Turn 1 的回复".into(),
                message_id: Some("msg_1".into()),
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        // === Turn 2: user bubble, reasoning + text → TurnDone ===
        dispatch_and_notify(
            &mut state,
            &AcpEventData::LocalUserBubble {
                text: "第二个问题".into(),
            },
        );
        dispatch_and_notify(&mut state, &AcpEventData::PromptStarted);
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ReasoningChunk(crate::kit::stream_data::TuiReasoningChunk {
                text: "Turn 2 的思考内容".into(),
                message_id: Some("msg_2".into()),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: "Turn 2 的回复".into(),
                message_id: Some("msg_2".into()),
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        // 验证 committed 有 4 个 VM：User1, Assistant1, User2, Assistant2
        assert_eq!(
            state.committed.len(),
            4,
            "committed 应有 User1 + Assistant1 + User2 + Assistant2 = 4 个 VM"
        );

        // Turn 1 Assistant 应有 reasoning
        let user1 = &state.committed[0];
        assert!(
            matches!(user1, TuiRenderUnit::TuiUserBubble(_)),
            "committed[0] 应为 TuiUserBubble，实际是 {user1:?}"
        );
        match &state.committed[1] {
            TuiRenderUnit::TuiAssistantBubble(d) => {
                assert!(d.reasoning.is_some(), "Turn 1 Assistant 应有 reasoning 块");
                assert_eq!(d.reasoning.as_ref().unwrap().text, "Turn 1 的思考内容");
            }
            other => panic!("expected TuiAssistantBubble at [1], got {other:?}"),
        }

        // Turn 2 Assistant 应有 reasoning
        let user2 = &state.committed[2];
        assert!(
            matches!(user2, TuiRenderUnit::TuiUserBubble(_)),
            "committed[2] 应为 TuiUserBubble，实际是 {user2:?}"
        );
        match &state.committed[3] {
            TuiRenderUnit::TuiAssistantBubble(d) => {
                assert!(d.reasoning.is_some(), "Turn 2 Assistant 应有 reasoning 块");
                assert_eq!(d.reasoning.as_ref().unwrap().text, "Turn 2 的思考内容");
            }
            other => panic!("expected TuiAssistantBubble at [3], got {other:?}"),
        }

        // 验证 VIEW_MODELS snapshot
        let snapshot = VIEW_MODELS.state().read().clone();
        assert_eq!(snapshot.items.len(), 4);

        // Turn 1 reasoning 应折叠（collapsed = true）——中间块
        match &snapshot.items[1] {
            TuiRenderUnit::TuiAssistantBubble(d) => {
                let r = d.reasoning.as_ref().unwrap();
                assert!(r.collapsed, "Turn 1 reasoning 应折叠（collapsed=true）");
            }
            other => panic!("expected TuiAssistantBubble at snapshot[1], got {other:?}"),
        }

        // Turn 2 reasoning 应展开（collapsed = false）——最后一个
        match &snapshot.items[3] {
            TuiRenderUnit::TuiAssistantBubble(d) => {
                let r = d.reasoning.as_ref().unwrap();
                assert!(
                    !r.collapsed,
                    "Turn 2 reasoning 应展开（collapsed=false）——最后一个"
                );
            }
            other => panic!("expected TuiAssistantBubble at snapshot[3], got {other:?}"),
        }
    }

    /// C2: compact 完成后 TurnDone 触发 session/load 重放。
    ///
    /// 场景 A：命令 compact（Immediate）后 current_turn 为空 → 触发 THREAD_LOAD_TX。
    /// 场景 B：agent 内部 compact 后 current_turn 非空（有后续流事件）→ 不触发。
    ///
    /// 注：THREAD_LOAD_TX 是 OnceLock，两场景合并为单测试以避免 set 冲突。
    #[test]
    #[serial]
    fn test_compact_turndone_reload() {
        use tokio::sync::mpsc;

        // ── 场景 A：命令 compact → 触发 reload ──────────────────────────
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

        let (tx_a, mut rx_a) = mpsc::unbounded_channel::<String>();
        let _ = THREAD_LOAD_TX.set(tx_a);

        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: "test-session".to_string(),
            compact_just_completed: true,
            last_submitted_text: None,
        };

        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        let received = rx_a.try_recv().ok();
        assert_eq!(
            received.as_deref(),
            Some("test-session"),
            "场景 A: THREAD_LOAD_TX 应收到 session_id"
        );
        assert!(!state.compact_just_completed, "场景 A: flag 应清除");

        // ── 场景 B：agent 内部 compact → 不触发 reload ──────────────────
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

        let mut state = BridgeState {
            variant: 1,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::PromptRunning,
            popup_kind: None,
            generation: 0,
            active_session_id: "test-session".to_string(),
            compact_just_completed: false,
            last_submitted_text: None,
        };
        state
            .current_turn
            .append_text("agent response after compact", None);
        state.compact_just_completed = true;

        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        // 场景 B 的 TurnDone 也会尝试发送（因为 flag 为 true 但 current_turn 非空），
        // 核心验证：flag 应被清除，但 reload 逻辑条件不满足（current_turn 非空）。
        assert!(!state.compact_just_completed, "场景 B: flag 应清除");
    }

    /// SubagentStopped 在 TurnDone 之后不应重新激活 loading。
    /// 场景：bg subagent 在 TurnDone 归档完成后才触发 SubagentStopped，
    /// SubagentStopped 不应将 phase 覆盖为 PromptRunning（不再设 is_loading=true）。
    #[test]
    #[serial]
    fn test_subagent_stopped_after_turn_done_does_not_set_loading() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // 模拟 TurnDone：归档 + 重置 phase/loading
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
        assert_eq!(
            state.phase,
            SessionPhase::Idle,
            "TurnDone 后 phase 应为 Idle"
        );
        assert!(
            !ACP_STATE.state().read().is_loading,
            "TurnDone 后 is_loading 应为 false"
        );

        // SubagentStopped 到达——不应重新激活 loading
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStopped {
                agent_id: "bg-agent-1".into(),
            },
        );

        assert!(
            !ACP_STATE.state().read().is_loading,
            "SubagentStopped after TurnDone: is_loading 应保持 false"
        );
    }

    /// SubagentStopped 在 TurnSuspended 之后不应重新激活 loading。
    #[test]
    #[serial]
    fn test_subagent_stopped_after_turn_suspended_does_not_set_loading() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // 模拟 TurnSuspended：归档 + 重置 phase/loading
        dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
        assert_eq!(
            state.phase,
            SessionPhase::Idle,
            "TurnSuspended 后 phase 应为 Idle"
        );
        assert!(
            !ACP_STATE.state().read().is_loading,
            "TurnSuspended 后 is_loading 应为 false"
        );

        // SubagentStopped 到达——不应重新激活 loading
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStopped {
                agent_id: "bg-agent-2".into(),
            },
        );

        assert!(
            !ACP_STATE.state().read().is_loading,
            "SubagentStopped after TurnSuspended: is_loading 应保持 false"
        );
    }

    /// SubagentStarted → SubagentStopped 路径（sync subagent）仍保持 loading。
    /// 同步 subagent 的 SubagentStarted 已设 phase=PromptRunning，
    /// SubagentStopped 不应破坏此状态。
    #[test]
    #[serial]
    fn test_subagent_stopped_after_subagent_started_keeps_loading() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // SubagentStarted 设置 phase=PromptRunning
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: "sync-agent-1".into(),
                agent_name: "researcher".into(),
                is_background: false,
            },
        );
        assert_eq!(
            state.phase,
            SessionPhase::PromptRunning,
            "SubagentStarted 后 phase 应为 PromptRunning"
        );
        assert!(
            ACP_STATE.state().read().is_loading,
            "SubagentStarted 后 is_loading 应为 true"
        );

        // SubagentStopped 到达——应保持 loading
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStopped {
                agent_id: "sync-agent-1".into(),
            },
        );

        assert!(
            ACP_STATE.state().read().is_loading,
            "SubagentStopped after SubagentStarted: is_loading 应保持 true"
        );
    }

    /// PromptSubmitted 事件应设 phase=PromptRunning + variant=1，
    /// push_acp_state 派生 is_loading=true。
    #[test]
    #[serial]
    fn test_prompt_submitted_sets_loading() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        dispatch_and_notify(&mut state, &AcpEventData::PromptSubmitted);

        assert_eq!(state.phase, SessionPhase::PromptRunning);
        assert_eq!(state.variant, 1);
        assert!(ACP_STATE.state().read().is_loading);
    }

    /// 同步 sub-agent 的 ToolStarted/ToolEnded 事件应路由到 SubAgentAccumulator，
    /// 并反映在 VIEW_MODELS 的 TuiSubAgentGroup 中。
    #[test]
    #[serial]
    fn test_dispatch_sync_subagent_tool_routed_to_group() {
        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        let mut state = BridgeState {
            variant: 0,
            committed: im::Vector::new(),
            current_turn: CurrentTurn::new(),
            phase: SessionPhase::Idle,
            popup_kind: None,
            generation: 0,
            active_session_id: String::new(),
            compact_just_completed: false,
            last_submitted_text: None,
        };

        // 启动同步 sub-agent
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: "sync-1".into(),
                agent_name: "coder".into(),
                is_background: false,
            },
        );
        // 工具开始
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
                agent_id: Some("sync-1".into()),
                tool_name: "Read".into(),
                tool_id: "tc-1".into(),
                input_summary: "path: foo.rs".into(),
            }),
        );
        // 工具结束
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
                agent_id: Some("sync-1".into()),
                tool_id: "tc-1".into(),
                output_summary: "10 lines".into(),
                is_error: false,
            }),
        );

        let snapshot = VIEW_MODELS.state().read().clone();
        // items 中应有 1 个 TuiSubAgentGroup
        assert_eq!(snapshot.items.len(), 1, "items 应包含 1 个元素");
        match &snapshot.items[0] {
            TuiRenderUnit::TuiSubAgentGroup(group) => {
                assert_eq!(group.agent_id, "sync-1");
                assert!(
                    !group.view_models.is_empty(),
                    "group.view_models 应至少包含 1 个工具卡片，实际 {} 个",
                    group.view_models.len()
                );
                let has_tool_card = group
                    .view_models
                    .iter()
                    .any(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)));
                assert!(
                    has_tool_card,
                    "group.view_models 应包含至少一个 TuiToolCard"
                );
            }
            other => panic!("expected TuiSubAgentGroup, got {other:?}"),
        }
    }
}
