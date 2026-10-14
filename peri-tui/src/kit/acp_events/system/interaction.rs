//! Interaction 事件处理器 — HitlPending / AskUser 弹窗与授权块构建。
//!
//! 由 `system` 挂载；`system::handle_*` / `system::hitl_input_summary` 路径经 `pub use` 保持不变。

use super::*;
use crate::i18n;
use crate::kit::acp_types::PendingInteraction;
use crate::kit::atoms::ASK_USER_PENDING;
use crate::kit::tui_render_unit::{InteractionKind, TuiAskUserBlock, TuiRenderUnit};
use fluent_bundle::FluentValue;
use peri_acp_types::event_data::{AskUser, HitlPending};
use serde_json::Value;

pub(crate) fn handle_hitl_pending(
    state: &mut BridgeState,
    pending: &PendingInteraction<HitlPending>,
) {
    // I21-A：保存 payload 到 HITL_PENDING atom，供 HitlPopup 读取真实数据
    *crate::kit::atoms::HITL_PENDING.state().write() = Some(pending.clone());
    state.popup_kind = Some(crate::kit::atoms::PopupKind::Hitl);
    state.variant = 2;
    // [Slice 4 §6.8] 双轨：inline transcript block（可见 + 可聚焦 + 结果回写）
    // 与 HITL 弹窗（模态操作层）并存。block 按事件到达位置 push 到 committed
    // ——不进 CurrentTurn 缓存（sync_cache 段对齐不可破坏）。
    let block = build_permission_block(pending);
    // [§6.8 模态互斥] 同 request_id 的 pending block 已存在（事件重放/重连/
    // 重试重复到达）→ 跳过注入——重复 pending 块永远不会被 resolve（单响应
    // 事件只匹配首个），会以「可聚焦假象」永久滞留 transcript。
    if !committed_has_pending(state, &pending.owner) {
        state
            .committed
            .push_back(TuiRenderUnit::TuiAskUserBlock(block));
    }
    state.publish_barrier();
    crate::kit::acp_events::render::push_popup_kind(state);
    crate::kit::acp_events::render::push_acp_state(state);
}

pub(crate) fn handle_ask_user(state: &mut BridgeState, pending: &PendingInteraction<AskUser>) {
    // I21-B：保存 payload 到 ASK_USER_PENDING atom，供 AskUserPanel 读取真实数据。
    // 通过 panel_registry 打开 AskUser 面板（非弹窗），内联在 MessageArea 下方。
    *ASK_USER_PENDING.state().write() = Some(pending.clone());
    crate::kit::panel_registry::open_panel(crate::app::panel_types::PanelKind::AskUser);
    state.variant = 2;
    // [Slice 4 §6.8] 双轨：inline transcript block 与 AskUser 面板（模态操作层）
    // 并存；block push 到 committed（时序即事件到达位置）。
    let block = build_ask_user_block(pending);
    // [§6.8 模态互斥] 同 request_id 的 pending block 已存在 → 跳过注入
    // （重复 pending 块不会被 resolve，永久滞留）。
    if !committed_has_pending(state, &pending.owner) {
        state
            .committed
            .push_back(TuiRenderUnit::TuiAskUserBlock(block));
    }
    state.publish_barrier();
    crate::kit::acp_events::render::push_acp_state(state);
}

/// [§6.8] committed 中是否已存在同 request_id 的 pending interaction block。
/// request_id 缺失（测试构造/协议异常）时按「无 pending 同源块」处理（不拦截）。
fn committed_has_pending(state: &BridgeState, owner: &crate::acp_client::InteractionOwner) -> bool {
    state.committed.iter().any(|vm| {
        matches!(vm, TuiRenderUnit::TuiAskUserBlock(a) if a.pending && a.owner.as_ref() == Some(owner))
    })
}

/// §6.8 结果回写（Slice 4）：扫描 committed 中 pending 的 interaction block，
/// 按 `request_id` 匹配 → clone + `pending=false` + `result` + `recompute_hash`，
/// 再原位 `set`（im::Vector COW）。匹配不到时 no-op（防御：本地事件迟到 /
/// 重复到达幂等）。
pub(crate) fn handle_interaction_terminal(
    state: &mut BridgeState,
    owner: &crate::acp_client::InteractionOwner,
    outcome: &crate::acp_client::InteractionUiOutcome,
) {
    let result = match outcome {
        crate::acp_client::InteractionUiOutcome::Resolved { result } => result.clone(),
        crate::acp_client::InteractionUiOutcome::Expired { .. } => {
            i18n::tr("render-interaction-result-rejected")
        }
    };
    {
        let atom = crate::kit::atoms::HITL_PENDING.state();
        let mut pending = atom.write();
        if pending
            .as_ref()
            .is_some_and(|pending| pending.owner == *owner)
        {
            *pending = None;
            if *crate::kit::atoms::POPUP_KIND.state().read()
                == Some(crate::kit::atoms::PopupKind::Hitl)
            {
                *crate::kit::atoms::POPUP_KIND.state().write() = None;
            }
        }
    }
    {
        let atom = ASK_USER_PENDING.state();
        let mut pending = atom.write();
        if pending
            .as_ref()
            .is_some_and(|pending| pending.owner == *owner)
        {
            *pending = None;
            crate::kit::panel_registry::close_panel(crate::app::panel_types::PanelKind::AskUser);
            if *crate::kit::atoms::POPUP_KIND.state().read()
                == Some(crate::kit::atoms::PopupKind::AskUser)
            {
                *crate::kit::atoms::POPUP_KIND.state().write() = None;
            }
        }
    }
    {
        let atom = crate::kit::atoms::CONFIRM_PAYLOAD.state();
        let mut confirm = atom.write();
        if confirm.as_ref().is_some_and(|payload| {
            matches!(
                &payload.pending_action,
                crate::kit::atoms::ConfirmAction::RejectAskUser { owner: pending_owner, .. }
                    if pending_owner == owner
            )
        }) {
            *confirm = None;
            if *crate::kit::atoms::POPUP_KIND.state().read()
                == Some(crate::kit::atoms::PopupKind::Confirm)
            {
                *crate::kit::atoms::POPUP_KIND.state().write() = None;
            }
        }
    }
    let mut updated: Option<(usize, TuiRenderUnit)> = None;
    for (i, vm) in state.committed.iter().enumerate() {
        if let TuiRenderUnit::TuiAskUserBlock(a) = vm
            && a.pending
            && a.owner.as_ref() == Some(owner)
        {
            let mut b = a.clone();
            b.pending = false;
            b.result = Some(result.clone());
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
    state.publish_barrier();
    crate::kit::acp_events::render::push_acp_state(state);
}

/// 构造 Permission（HITL）interaction block（§6.8）：
/// `! Approval required` / `{verb} wants to run: {input_summary}` /
/// `[Allow once] [Deny]`（D6：`[Always allow]` 为协议依赖项，不渲染）。
/// `request_id` 与 payload 从同一个 composite event 取得。
fn build_permission_block(pending: &PendingInteraction<HitlPending>) -> TuiAskUserBlock {
    let hp = &pending.payload;
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
        request_id: Some(pending.request_id_json.clone()),
        owner: Some(pending.owner.clone()),
        question_ids: Vec::new(),
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
fn build_ask_user_block(pending: &PendingInteraction<AskUser>) -> TuiAskUserBlock {
    let au = &pending.payload;
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
        request_id: Some(pending.request_id_json.clone()),
        owner: Some(pending.owner.clone()),
        question_ids: au.questions.iter().map(|q| q.id.clone()).collect(),
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
