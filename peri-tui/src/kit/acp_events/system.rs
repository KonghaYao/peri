//! System event handlers — BudgetWarning, SystemNotification, Prediction,
//! FileSuggestions, Interaction events (HitlPending, AskUser, RewindPreview,
//! RewindCompleted, OauthNeeded), Background Tasks, Plugin events, WorkflowProgress,
//! Unknown.
//!
//! 模块结构：Interaction 事件（HitlPending / AskUser 弹窗与授权块）见 `interaction`，
//! Background Tasks（快照 / 增量与任务生命周期）见 `bg_task`；系统提示与提醒、
//! rewind / OAuth 结果、插件与 workflow 事件留在本模块。

use super::*;
use crate::i18n;
use crate::kit::acp_types::{FeedbackChannel, FeedbackLevel, TuiCommandFeedback};
use crate::kit::atoms::PluginSummary;
use crate::kit::atoms::{NOTIFICATION, PLUGIN_LIST, PREDICTION, RENDER_HEARTBEAT};
use crate::kit::tui_render_unit::{
    DisplayTrusted, ReminderRelay, TuiNoteLevel, TuiRenderUnit, TuiSystemReminder,
};
use fluent_bundle::FluentValue;
use peri_acp_types::event_data::{
    BudgetWarning, OauthNeeded, PluginActionResult, PluginSnapshot, Prediction, PredictionAction,
    RewindMessage, RewindPreview, SystemNotification,
};
use serde_json::Value;
use std::time::Duration;

// ── Sub-modules ──
mod bg_task;
mod interaction;

// ── Re-exports ──
// 搬移后保持既有调用路径可用：`system::handle_bg_task_*` / `system::request_bg_task_snapshot`、
// `system::handle_{hitl_pending,ask_user,interaction_terminal}`（生产调用方：本模块 `acp_events/mod.rs`）。
pub(crate) use self::bg_task::request_bg_task_snapshot;
pub(super) use self::bg_task::{
    handle_bg_task_cancelled, handle_bg_task_completed, handle_bg_task_snapshot,
    handle_bg_task_started, handle_bg_task_updated,
};
pub(super) use self::interaction::{
    handle_ask_user, handle_hitl_pending, handle_interaction_terminal,
};

// 仅测试引用的既有路径（`acp_events_test/bg_task_live_test.rs`、`interaction_test.rs`）；
// 按 `acp_events/mod.rs` 既有先例用 `#[cfg(test)]` 门控，非测试构建不产生空转导入。
#[cfg(test)]
pub(crate) use self::bg_task::apply_bg_task_snapshot;
#[cfg(test)]
pub(crate) use self::interaction::hitl_input_summary;

pub(super) fn handle_budget_warning(state: &mut BridgeState, bw: &BudgetWarning) {
    // 上下文使用率超过阈值警告——注入 TuiSystemNote 到 current_turn 内部。
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
    state.inject_system_note(text, TuiNoteLevel::Warning);
}

pub(super) fn handle_llm_retrying(
    state: &mut BridgeState,
    attempt: usize,
    max_attempts: usize,
    delay_ms: u64,
    error: &str,
) {
    let delay_seconds = format!("{:.1}", delay_ms as f64 / 1000.0);
    let text = i18n::tr_args(
        "statusbar-retrying",
        &[
            ("attempt".into(), FluentValue::from(attempt as u64)),
            ("max".into(), FluentValue::from(max_attempts as u64)),
            ("delay".into(), FluentValue::from(delay_seconds)),
            ("error".into(), FluentValue::from(error)),
        ],
    );
    state.inject_system_note(text, TuiNoteLevel::Warning);
}

pub(super) fn handle_goal_snapshot(
    objective: &Option<String>,
    status: Option<peri_acp_types::goal::GoalStatus>,
    token_budget: Option<u64>,
    tokens_used: u64,
    time_used_seconds: u64,
    continuation_count: u64,
    blocked_reason: &Option<String>,
) {
    *crate::kit::atoms::GOAL_SNAPSHOT.state().write() =
        status.map(|status| crate::kit::atoms::GoalSnapshot {
            objective: objective.clone(),
            status: Some(status),
            token_budget,
            tokens_used,
            time_used_seconds,
            continuation_count,
            blocked_reason: blocked_reason.clone(),
        });
}

/// 解析转发来源展示归属：子 Agent 名优先，TUI 未登记时只保留 child_thread_id。
fn resolve_reminder_relay(
    state: &BridgeState,
    source_agent_id: Option<&str>,
) -> Option<ReminderRelay> {
    let agent_id = source_agent_id?;
    Some(ReminderRelay {
        agent_id: agent_id.to_string(),
        agent_name: state
            .current_turn
            .subagent_name(agent_id)
            .map(str::to_string),
    })
}

pub(super) fn handle_trusted_system_reminder(
    state: &mut BridgeState,
    reminder: peri_acp_types::system_reminder::SystemReminder,
    source_agent_id: Option<&str>,
) {
    if let Some(mut vm) = TuiSystemReminder::from_trusted_structured(
        DisplayTrusted::after_current_session_gate(reminder),
    ) {
        vm.attach_relay(resolve_reminder_relay(state, source_agent_id));
        state.flush_current_turn();
        state
            .committed
            .push_back(TuiRenderUnit::TuiSystemReminder(vm));
    }
}

pub(super) fn handle_system_reminder(
    state: &mut BridgeState,
    reminder: &peri_acp_types::system_reminder::SystemReminder,
    source_agent_id: Option<&str>,
) {
    if let Some(mut vm) = TuiSystemReminder::from_wire(reminder.clone()) {
        vm.attach_relay(resolve_reminder_relay(state, source_agent_id));
        state.flush_current_turn();
        state
            .committed
            .push_back(TuiRenderUnit::TuiSystemReminder(vm));
    }
}

pub(super) fn handle_system_reminder_fallback(state: &mut BridgeState, text: &str) {
    state.flush_current_turn();
    state
        .committed
        .push_back(TuiRenderUnit::TuiSystemReminder(TuiSystemReminder::legacy(
            text.to_string(),
        )));
}

pub(super) fn handle_system_notification(state: &mut BridgeState, sn: &SystemNotification) {
    // 系统通知（如 cache 命中率警告）——通过 inject_system_note 注入
    // current_turn 内部，天然位于其时序位置（已产出内容之后、后续内容之前）。
    let level = match sn.level.as_str() {
        "warning" => TuiNoteLevel::Warning,
        "error" => TuiNoteLevel::Error,
        _ => TuiNoteLevel::Info,
    };
    state.inject_system_note(sn.text.clone(), level);
}

pub(super) fn handle_command_feedback(state: &mut BridgeState, fb: &TuiCommandFeedback) {
    // CompactCompleted 已注入更完整的展示信息；手动 compact 紧随其后的通用
    // CommandFeedback 仅是兼容反馈，跳过以避免消息流短暂出现两条完成通知。
    if fb.channel == FeedbackChannel::UiOnly
        && state.compact_just_completed
        && crate::kit::atoms::PENDING_COMPACT_NOTE
            .state()
            .read()
            .is_some()
    {
        return;
    }
    // 命令执行反馈（Phase 3 CommandFeedback 事件）。v1 不建独立通知条组件，
    // UiOnly/Session 两通道均先走 inject_system_note——SystemNote 是 TUI 渲染
    // 层概念，不进 ACP 消息，agent 永不见（设计 §79）；通知条/状态区的视觉
    // 差异留待后续组件化。
    // channel=Session 的会话写入（命令显式 opt-in 才另写系统消息进会话，
    // 设计 §79/§89）待通知条组件化（Phase 5+）时收口——此处保留显式分支，
    // 防止 Phase 3 侧 opt-in Session 的命令上线后反馈语义静默丢失而无编译期提醒。
    let level = match fb.level {
        FeedbackLevel::Info => TuiNoteLevel::Info,
        FeedbackLevel::Warning => TuiNoteLevel::Warning,
        FeedbackLevel::Error => TuiNoteLevel::Error,
    };
    match fb.channel {
        FeedbackChannel::UiOnly | FeedbackChannel::Session => {
            state.inject_system_note(fb.message.clone(), level);
        }
    }
    // compact 手动完成的 CommandFeedback SystemNote 可能被随后的
    // BRIDGE_RESET_COUNTER 重置清空（committed 整体重建）——aecc2834 的
    // PENDING_COMPACT_NOTE 桥接在文案移交 CommandFeedback 渲染时曾丢失该机制
    // （e2e compact-command waitFor 完成提示 120s 超时），这里恢复写入。仅 UiOnly
    // 且 compact 完成场景写入：其它命令不写，防残留串到后续 thread 切换的 reset。
    // （REQ-C02 / A12 之后 manual compact 不再触发 session/load replay，但 reset
    // 边界仍然存在。）
    if fb.channel == FeedbackChannel::UiOnly
        && state.compact_just_completed
        && crate::kit::atoms::PENDING_COMPACT_NOTE
            .state()
            .read()
            .is_none()
    {
        crate::kit::atoms::PENDING_COMPACT_NOTE.set(Some(fb.message.clone()));
    }
}

pub(super) fn handle_prediction(p: &Prediction) {
    let mut summary = None;
    let mut text = p.text.clone();
    for action in &p.actions {
        match action {
            PredictionAction::Placeholder { text: t } => text = t.clone(),
            PredictionAction::Summary { text: t } => summary = Some(t.clone()),
            _ => {} // SetTitle / AddTag 由 ACP host 执行写入，此处仅展示
        }
    }
    let mut state = crate::kit::atoms::PredictionState {
        text,
        summary,
        received_at: Some(peri_time::monotonic_now()),
    };
    if state.text.is_empty() {
        // 仅元数据动作（SetTitle/AddTag）的 prediction 不带占位文本——
        // 保留输入区现有占位，避免空文本覆盖已有预测内容。
        state.text = PREDICTION.state().read().text.clone();
    }
    *PREDICTION.state().write() = state;
}

pub(super) fn handle_file_suggestions() {}

pub(super) fn handle_rewind_completed(state: &mut BridgeState, messages_json: &str) {
    // rewind 改变了会话的有效范围：V2 历史由会话边界/下一次 initial 重建，
    // 详情面板内容属于旧窗口，必须显式关闭。
    crate::kit::atoms::FOCUSED_ENTRY.set(None);
    crate::kit::atoms::HISTORY_DETAIL.set(None);
    // H3: rewind 完成——反序列化 messages_json 为 Vec<Value>，按 role
    // 映射为 TuiRenderUnit，替换 state.committed。
    let messages: Result<Vec<Value>, _> = serde_json::from_str(messages_json);
    match messages {
        Ok(msgs) => {
            // rewind 改变了会话有效范围：旧窗口、在途请求与投影前缀全部作废，
            // 再重新 open 最新窗口。旧页/旧 open 响应由会话与窗口身份及
            // consumer epoch 拦截，不能回写到回退后的历史。
            crate::kit::history_v2::consumer::request_close_current();
            crate::kit::history_v2::reset_state();
            crate::kit::history_v2::consumer::reset_consumer_state();
            let session_id = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
            if !session_id.is_empty() {
                crate::kit::history_v2::consumer::request_open_latest(&session_id);
            }
            state.committed.clear();
            for msg in &msgs {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                match role {
                    "user" => {
                        let text = super::extract_message_text(msg);
                        state.committed.push_back(TuiRenderUnit::TuiUserBubble(
                            crate::kit::tui_render_unit::TuiUserBubble::new(text),
                        ));
                    }
                    "assistant" | "ai" => {
                        let text = super::extract_message_text(msg);
                        let content_hash =
                            crate::kit::tui_render_unit::tui_hash_str(&format!("{}|", text));
                        state.committed.push_back(TuiRenderUnit::TuiAssistantBubble(
                            crate::kit::tui_render_unit::TuiAssistantBubble {
                                text,
                                reasoning: None,
                                // 重放消息无 message_id 来源——不作为折叠覆盖目标。
                                message_id: None,
                                // 重放路径无流式时长起点——不显示 `12.4s`（G-Tokens）。
                                started_at: None,
                                duration_ms: None,
                                content_hash,
                            }
                            .into(),
                        ));
                    }
                    _ => {}
                }
            }
            // 同步重建 REWIND_PREVIEW：rewind 后消息列表已变，旧 preview 中的
            // 消息 id 已从服务端 history 删除——不重建会导致连续第二次回滚
            // 时 target 找不到（服务端 emit_rewind_not_found）。从回滚后的
            // 消息 JSON 直接提取 id/role/preview，保证候选列表与消息区一致。
            // P1：只保留 user 消息且排除系统注入（与 rewind-candidates 口径
            // 一致），并逆序（最新在前）——弹窗第一条 = 回退一步。
            // 口径统一：剥离 `<system-reminder>` 注入块后为空（纯系统注入）
            // 的消息不进候选；带尾部注入的用户输入剥离后保留（与服务端
            // rewind-candidates 行为一致，避免多轮场景候选不一致）。
            let preview = RewindPreview {
                files: vec![],
                messages: msgs
                    .iter()
                    .rev()
                    .filter_map(|msg| {
                        let id = msg.get("id").and_then(|v| v.as_str())?.to_string();
                        let role = msg
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let text = peri_acp_types::messages::strip_system_reminders(
                            &super::extract_message_text(msg),
                        );
                        let text = text.trim();
                        if role != "user" || text.is_empty() {
                            return None;
                        }
                        Some(RewindMessage {
                            id,
                            role,
                            preview: text.chars().take(200).collect(),
                        })
                    })
                    .collect(),
            };
            *crate::kit::atoms::REWIND_PREVIEW.state().write() = Some(preview);
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
    state.publish_barrier();

    // Rewind v2：回填目标 user 消息文本到输入框（复用 TurnInterrupted 的回填通道）。
    // 消费 REWIND_TARGET_TEXT → INPUT_RESTORE_TEXT + 心跳 → InputArea use_effect
    // 写入编辑态并替换草稿；焦点随 close_popup（RewindCompleted 到达前弹窗已由
    // consumer 流程关闭或停留在执行中态，此处统一关闭）。
    if let Some(target_text) = crate::kit::atoms::REWIND_TARGET_TEXT.state().write().take() {
        let mu =
            crate::kit::atoms::INPUT_RESTORE_TEXT.get_or_init(|| parking_lot::Mutex::new(None));
        *mu.lock() = Some(target_text);
        crate::kit::atoms::RENDER_HEARTBEAT
            .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
    }
    // 回退完成：预算状态复位、查询错误清空；弹窗关闭（执行完成）
    *crate::kit::atoms::REWIND_BUDGET_STATE.state().write() =
        crate::kit::atoms::RewindBudgetState::Idle;
    *crate::kit::atoms::REWIND_PREVIEW_FINGERPRINT
        .state()
        .write() = None;
    *crate::kit::atoms::REWIND_QUERY_ERROR.state().write() = None;
    // P1：仅当 rewind 弹窗仍在显示时关闭——执行期间用户可能已 Esc 关闭弹窗
    // 或打开了其他弹窗（HITL/OAuth 事件），无条件 close 会误关。
    if *crate::kit::atoms::POPUP_KIND.state().read() == Some(crate::kit::atoms::PopupKind::Rewind) {
        crate::kit::popup_overlay::close_popup();
    }

    super::render::push_acp_state(state);
}

pub(super) fn handle_oauth_needed(state: &mut BridgeState, on: &OauthNeeded) {
    // I20-D：保存 payload 到 OAUTH_INFO atom，供 OAuthPopup 读取真实数据
    *crate::kit::atoms::OAUTH_INFO.state().write() = Some(on.clone());
    crate::kit::atoms::OAUTH_SESSION_ID
        .set((!state.active_session_id.is_empty()).then(|| state.active_session_id.clone()));
    state.popup_kind = Some(crate::kit::atoms::PopupKind::OAuth);
    state.variant = 2;
    super::render::push_popup_kind(state);
    super::render::push_acp_state(state);
}

/// 关闭 OAuth popup 并清理 atom（Completed/Failed 共用）。
fn close_oauth_popup(state: &mut BridgeState) {
    state.popup_kind = None;
    *crate::kit::atoms::OAUTH_INFO.state().write() = None;
    crate::kit::atoms::OAUTH_SESSION_ID.set(None);
    super::render::push_popup_kind(state);
}

pub(super) fn handle_oauth_completed(state: &mut BridgeState, server_name: &str) {
    // 授权完成：关闭 popup 并提示（MCP 面板状态由 service_snapshot 轮询 mcp/list 刷新）
    close_oauth_popup(state);
    let text = i18n::tr_args(
        "mcp-oauth-completed",
        &[("server".into(), FluentValue::from(server_name))],
    );
    state.inject_system_note(text, TuiNoteLevel::Info);
    super::render::push_acp_state(state);
}

pub(super) fn handle_oauth_failed(state: &mut BridgeState, server_name: &str, error: &str) {
    // 授权失败（超时/取消/服务端拒绝）：关闭 popup 并提示原因
    close_oauth_popup(state);
    let text = i18n::tr_args(
        "mcp-oauth-failed",
        &[
            ("server".into(), FluentValue::from(server_name)),
            ("error".into(), FluentValue::from(error)),
        ],
    );
    state.inject_system_note(text, TuiNoteLevel::Warning);
    super::render::push_acp_state(state);
}

pub(super) fn handle_oauth_restored(state: &mut BridgeState, server_name: &str) {
    // 凭证恢复成功（快速路径）：用户主动发起授权但磁盘已有有效凭证，
    // 不弹 popup；提示「已使用已保存凭证连接」（面板状态经轮询刷新）。
    let text = i18n::tr_args(
        "mcp-oauth-restored",
        &[("server".into(), FluentValue::from(server_name))],
    );
    state.inject_system_note(text, TuiNoteLevel::Info);
    super::render::push_acp_state(state);
}

// ── §4.9 Plugin events ──

pub(super) fn handle_plugin_snapshot(snapshot: &PluginSnapshot) {
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
            install_scope: Some(p.install_scope.clone()),
            toggle_supported: Some(matches!(
                p.install_scope.as_str(),
                "user" | "project" | "local"
            )),
            management_error: None,
            load_error: p.load_error.clone(),
        })
        .collect();
    PLUGIN_LIST.state().write().clone_from(&plugins);
}

pub(super) fn handle_plugin_action_result(result: &PluginActionResult) {
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
    NOTIFICATION
        .state()
        .write()
        .replace(crate::kit::atoms::Notification {
            message: msg,
            until: peri_time::monotonic_now() + Duration::from_secs(3),
        });
    // 触发 PluginPanel 重渲染以清除 operation_loading
    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
}

// ── Other ──

pub(super) fn handle_workflow_progress(
    run_id: &str,
    workflow_name: &str,
    event_type: &str,
    phase: &Option<String>,
) {
    tracing::debug!(
        run_id,
        workflow_name,
        event_type,
        phase = ?phase,
        "bridge: WorkflowProgress"
    );
}

pub(super) fn handle_unknown() {}
