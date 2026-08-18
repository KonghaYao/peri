//! System event handlers — BudgetWarning, SystemNotification, Prediction,
//! FileSuggestions, Interaction events (HitlPending, AskUser, RewindPreview,
//! RewindCompleted, OauthNeeded), Background Tasks, Plugin events, WorkflowProgress,
//! Unknown.

use super::*;
use crate::i18n;
use crate::kit::acp_types::{BgTaskEntry, FeedbackChannel, FeedbackLevel, TuiCommandFeedback};
use crate::kit::atoms::PluginSummary;
use crate::kit::atoms::{
    ASK_USER_PENDING, ASK_USER_REQUEST_ID, BG_DISPLAY, BG_TASKS, HITL_REQUEST_ID, NOTIFICATION,
    PLUGIN_LIST, PLUGIN_SEARCH_RESULTS, PREDICTION, RENDER_HEARTBEAT,
};
use crate::kit::tui_render_unit::{InteractionKind, TuiAskUserBlock, TuiNoteLevel, TuiRenderUnit};
use fluent_bundle::FluentValue;
use peri_acp_types::event_data::{
    AskUser, BudgetWarning, HitlPending, OauthNeeded, PluginActionResult, PluginSearchResult,
    PluginSnapshot, Prediction, PredictionAction, RewindMessage, RewindPreview, SystemNotification,
};
use serde_json::Value;
use std::time::{Duration, Instant};

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
    // Phase 5 Step 7 补遗（Step 8 回归修复）：compact 手动完成的 CommandFeedback
    // SystemNote 会被紧随的 session/load replay（BRIDGE_RESET_COUNTER reset）
    // 清空——8/8 aecc2834 的 PENDING_COMPACT_NOTE 跨 replay 桥接随 Step 7
    // 删除后，文案移交 CommandFeedback 渲染时未保留该机制（e2e compact-command
    // waitFor 完成提示 120s 超时）。仅 UiOnly 且 compact 完成场景写入：auto
    // compact 无 replay 触发不写、非 compact 命令无 replay 不写，防残留串到
    // 后续 thread 切换的 reset。
    if fb.channel == FeedbackChannel::UiOnly && state.compact_just_completed {
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
        received_at: Some(Instant::now()),
    };
    if state.text.is_empty() {
        // 仅元数据动作（SetTitle/AddTag）的 prediction 不带占位文本——
        // 保留输入区现有占位，避免空文本覆盖已有预测内容。
        state.text = PREDICTION.state().read().text.clone();
    }
    *PREDICTION.state().write() = state;
}

pub(super) fn handle_file_suggestions() {}

pub(super) fn handle_hitl_pending(state: &mut BridgeState, hp: &HitlPending) {
    // I21-A：保存 payload 到 HITL_PENDING atom，供 HitlPopup 读取真实数据
    *crate::kit::atoms::HITL_PENDING.state().write() = Some(hp.clone());
    state.popup_kind = Some(crate::kit::atoms::PopupKind::Hitl);
    state.variant = 2;
    // [Slice 4 §6.8] 双轨：inline transcript block（可见 + 可聚焦 + 结果回写）
    // 与 HITL 弹窗（模态操作层）并存。block 按事件到达位置 push 到 committed
    // ——不进 CurrentTurn 缓存（sync_cache 段对齐不可破坏）。
    let block = build_permission_block(hp);
    // [§6.8 模态互斥] 同 request_id 的 pending block 已存在（事件重放/重连/
    // 重试重复到达）→ 跳过注入——重复 pending 块永远不会被 resolve（单响应
    // 事件只匹配首个），会以「可聚焦假象」永久滞留 transcript。
    if !committed_has_pending(state, block.request_id.as_deref()) {
        state
            .committed
            .push_back(TuiRenderUnit::TuiAskUserBlock(block));
    }
    super::render::push_view_models(state);
    super::render::push_popup_kind(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_ask_user(state: &mut BridgeState, au: &AskUser) {
    // I21-B：保存 payload 到 ASK_USER_PENDING atom，供 AskUserPanel 读取真实数据。
    // 通过 panel_registry 打开 AskUser 面板（非弹窗），内联在 MessageArea 下方。
    *ASK_USER_PENDING.state().write() = Some(au.clone());
    crate::kit::panel_registry::open_panel(crate::app::panel_types::PanelKind::AskUser);
    state.variant = 2;
    // [Slice 4 §6.8] 双轨：inline transcript block 与 AskUser 面板（模态操作层）
    // 并存；block push 到 committed（时序即事件到达位置）。
    let block = build_ask_user_block(au);
    // [§6.8 模态互斥] 同 request_id 的 pending block 已存在 → 跳过注入
    // （重复 pending 块不会被 resolve，永久滞留）。
    if !committed_has_pending(state, block.request_id.as_deref()) {
        state
            .committed
            .push_back(TuiRenderUnit::TuiAskUserBlock(block));
    }
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

/// [§6.8] committed 中是否已存在同 request_id 的 pending interaction block。
/// request_id 缺失（测试构造/协议异常）时按「无 pending 同源块」处理（不拦截）。
fn committed_has_pending(state: &BridgeState, request_id: Option<&str>) -> bool {
    let Some(id) = request_id else {
        return false;
    };
    state.committed.iter().any(|vm| {
        matches!(vm, TuiRenderUnit::TuiAskUserBlock(a) if a.pending && a.request_id.as_deref() == Some(id))
    })
}

/// §6.8 结果回写（Slice 4）：扫描 committed 中 pending 的 interaction block，
/// 按 `request_id` 匹配 → clone + `pending=false` + `result` + `recompute_hash`，
/// 再原位 `set`（im::Vector COW）。匹配不到时 no-op（防御：本地事件迟到 /
/// 重复到达幂等）。
pub(super) fn handle_interaction_resolved(state: &mut BridgeState, request_id: &str, result: &str) {
    let mut updated: Option<(usize, TuiRenderUnit)> = None;
    for (i, vm) in state.committed.iter().enumerate() {
        if let TuiRenderUnit::TuiAskUserBlock(a) = vm
            && a.pending
            && a.request_id.as_deref() == Some(request_id)
        {
            let mut b = a.clone();
            b.pending = false;
            b.result = Some(result.to_string());
            // 结果回写后收束为结果行（§7 completed → Collapsed）——
            // 手动展开的覆盖由折叠 pass 依据 FOLD_OVERRIDES 恢复。
            if !b.user_modified {
                b.fold = crate::kit::tui_render_unit::fold_for_status(
                    crate::kit::tui_render_unit::FoldTarget::Interaction,
                    crate::kit::tui_render_unit::EntryStatus::Completed,
                );
            }
            b.recompute_hash();
            updated = Some((i, TuiRenderUnit::TuiAskUserBlock(b)));
            break;
        }
    }
    if let Some((i, vm)) = updated {
        state.committed.set(i, vm);
    }
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

/// 构造 Permission（HITL）interaction block（§6.8）：
/// `! Approval required` / `{verb} wants to run: {input_summary}` /
/// `[Allow once] [Deny]`（D6：`[Always allow]` 为协议依赖项，不渲染）。
/// `request_id` 从 HITL_REQUEST_ID atom 克隆（acp_notifier 在发事件前写入）。
fn build_permission_block(hp: &HitlPending) -> TuiAskUserBlock {
    let mut verb = hp.tool_name.clone();
    if verb.is_empty() {
        verb = i18n::tr("render-interaction-tool-unknown");
    }
    let question = i18n::tr_args(
        "render-interaction-question-permission",
        &[
            ("verb".to_string(), FluentValue::from(verb.as_str())),
            (
                "summary".to_string(),
                FluentValue::from(hitl_input_summary(&hp.tool_input).as_str()),
            ),
        ],
    );
    let mut block = TuiAskUserBlock {
        items: Vec::new(),
        is_error: false,
        kind: InteractionKind::Permission,
        pending: true,
        verb,
        question,
        options: vec![
            i18n::tr("render-interaction-allow-once"),
            i18n::tr("render-interaction-deny"),
        ],
        result: None,
        request_id: HITL_REQUEST_ID.state().read().clone(),
        fold: crate::kit::tui_render_unit::FoldState::Expanded,
        user_modified: false,
        content_hash: 0,
    };
    block.recompute_hash();
    block
}

/// 构造 AskUser interaction block（§6.8）：首问 header/options 摘要。
/// 多问题表单的完整编辑保留在 AskUser 面板（双轨 D5）；inline 只承担首问
/// 的快速回答（其余问题提交空字符串，协议结构完整）。
fn build_ask_user_block(au: &AskUser) -> TuiAskUserBlock {
    let first = au.questions.first();
    let question = match first {
        Some(q) if !q.header.is_empty() => q.header.clone(),
        Some(q) if !q.question.is_empty() => q.question.clone(),
        _ => i18n::tr("render-interaction-title-ask-user"),
    };
    let options: Vec<String> = first
        .map(|q| q.options.iter().map(|o| o.label.clone()).collect())
        .unwrap_or_default();
    let mut block = TuiAskUserBlock {
        items: Vec::new(),
        is_error: false,
        kind: InteractionKind::AskUser,
        pending: true,
        verb: "AskUser".to_string(),
        question,
        options,
        result: None,
        request_id: ASK_USER_REQUEST_ID.state().read().clone(),
        fold: crate::kit::tui_render_unit::FoldState::Expanded,
        user_modified: false,
        content_hash: 0,
    };
    block.recompute_hash();
    block
}

/// 从 HITL tool_input raw JSON 提取人类可读的输入摘要。
/// 优先提取工具主要对象字段（command/path/query/url/pattern），
/// fallback 紧凑 JSON——hitl_popup 的 pretty JSON 保留为弹窗展示。
pub(crate) fn hitl_input_summary(input: &Value) -> String {
    if let Some(obj) = input.as_object() {
        for key in [
            "command",
            "path",
            "query",
            "url",
            "file_path",
            "pattern",
            "name",
            "description",
        ] {
            if let Some(v) = obj.get(key)
                && let Some(s) = v.as_str()
                && !s.is_empty()
            {
                return s.to_string();
            }
        }
    }
    match serde_json::to_string(input) {
        Ok(s) if !s.is_empty() && s != "null" => s,
        _ => i18n::tr("render-interaction-tool-unknown"),
    }
}

pub(super) fn handle_rewind_completed(state: &mut BridgeState, messages_json: &str) {
    // H3: rewind 完成——反序列化 messages_json 为 Vec<Value>，按 role
    // 映射为 TuiRenderUnit，替换 state.committed。
    let messages: Result<Vec<Value>, _> = serde_json::from_str(messages_json);
    match messages {
        Ok(msgs) => {
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
                            },
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
    super::render::push_view_models(state);

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
    state.popup_kind = Some(crate::kit::atoms::PopupKind::OAuth);
    state.variant = 2;
    super::render::push_popup_kind(state);
    super::render::push_acp_state(state);
}

/// 关闭 OAuth popup 并清理 atom（Completed/Failed 共用）。
fn close_oauth_popup(state: &mut BridgeState) {
    state.popup_kind = None;
    *crate::kit::atoms::OAUTH_INFO.state().write() = None;
    super::render::push_popup_kind(state);
}

pub(super) fn handle_oauth_completed(state: &mut BridgeState, server_name: &str) {
    // 授权完成：关闭 popup 并提示（MCP 面板状态由 pool 侧更新）
    close_oauth_popup(state);
    let text = i18n::tr_args(
        "mcp-oauth-completed",
        &[("server".into(), FluentValue::from(server_name))],
    );
    state.inject_system_note(text, TuiNoteLevel::Info);
    super::render::push_acp_state(state);

    // TUI 面板直读 pool：授权凭证已落盘（host pool 完成），触发该 server
    // reconnect 恢复连接（reconnect 内部走凭证快速路径，不重复弹授权）。
    if let Some(pool) = crate::kit::atoms::MCP_PANEL_POOL.get() {
        let pool = pool.clone();
        let name = server_name.to_string();
        tokio::spawn(async move {
            if let Err(e) = pool.reconnect(&name, None).await {
                tracing::warn!(server = %name, error = %e, "面板 MCP 授权后重连失败");
            }
        });
    }
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
    // 不弹 popup；提示「已使用已保存凭证连接」并同步面板池状态。
    let text = i18n::tr_args(
        "mcp-oauth-restored",
        &[("server".into(), FluentValue::from(server_name))],
    );
    state.inject_system_note(text, TuiNoteLevel::Info);
    super::render::push_acp_state(state);

    // 同步 TUI 面板池（reconnect 走凭证快速路径，不重复弹授权）。
    if let Some(pool) = crate::kit::atoms::MCP_PANEL_POOL.get() {
        let pool = pool.clone();
        let name = server_name.to_string();
        tokio::spawn(async move {
            if let Err(e) = pool.reconnect(&name, None).await {
                tracing::warn!(server = %name, error = %e, "面板 MCP 凭证恢复后重连失败");
            }
        });
    }
}

// ── §4.7 Background Tasks ──

pub(super) fn handle_bg_task_snapshot(state: &mut BridgeState, tasks: &[BgTaskEntry]) {
    let tasks_vec: Vec<BgTaskEntry> = tasks.to_vec();
    BG_TASKS.state().write().clone_from(&tasks_vec);
    // 从快照全量构造 BG_DISPLAY 条目
    let entries: Vec<crate::kit::atoms::BgDisplayEntry> = tasks
        .iter()
        .map(|t| crate::kit::atoms::BgDisplayEntry {
            id: t.task_id.clone(),
            agent_type: t.kind.clone(),
            desc: t.summary.clone(),
            is_active: true,
            is_error: false,
            current_tool: None,
            tool_count: 0,
            created_at: Instant::now(),
            completed_at: None,
        })
        .collect();
    BG_DISPLAY.state().write().clone_from(&entries);
    super::render::push_acp_state(state);
}

pub(super) fn handle_bg_task_started(_state: &mut BridgeState, entry: &BgTaskEntry) {
    BG_TASKS.state().write().push(entry.clone());
    let display_entry = crate::kit::atoms::BgDisplayEntry {
        id: entry.task_id.clone(),
        agent_type: entry.kind.clone(),
        desc: entry.summary.clone(),
        is_active: true,
        is_error: false,
        current_tool: None,
        tool_count: 0,
        created_at: Instant::now(),
        completed_at: None,
    };
    BG_DISPLAY.state().write().push(display_entry);
}

pub(super) fn handle_bg_task_completed(task_id: &str, success: bool, duration_ms: u64) {
    BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
    // 标记后台显示条目为完成（保留 3s 后自动清除）
    let now = Instant::now();
    if let Some(entry) = BG_DISPLAY
        .state()
        .write()
        .iter_mut()
        .find(|e| e.id == *task_id)
    {
        entry.is_active = false;
        entry.is_error = !success;
        entry.completed_at = Some(now);
    }
    let msg = if success {
        i18n::tr_args(
            "app-note-bg-task-completed",
            &[(
                "duration".into(),
                FluentValue::from(duration_ms as f64 / 1000.0),
            )],
        )
    } else {
        i18n::tr_args(
            "app-note-bg-task-failed",
            &[(
                "duration".into(),
                FluentValue::from(duration_ms as f64 / 1000.0),
            )],
        )
    };
    NOTIFICATION
        .state()
        .write()
        .replace(crate::kit::atoms::Notification {
            message: msg,
            until: Instant::now() + Duration::from_millis(1500),
        });
}

pub(super) fn handle_bg_task_cancelled(task_id: &str) {
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
            install_scope: p.install_scope.clone(),
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
            until: Instant::now() + Duration::from_secs(3),
        });
    // 触发 PluginPanel 重渲染以清除 operation_loading
    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
}

pub(super) fn handle_plugin_search_result(result: &PluginSearchResult) {
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
