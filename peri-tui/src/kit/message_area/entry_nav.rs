use super::scroll::GesturePending;
use crate::kit::atoms::{
    FOCUSED_ENTRY, FOLD_OVERRIDES, FocusedEntry, SELECTED_SUBAGENT_ID, ViewModelsSnapshot,
};
use crate::kit::tui_render_unit::{
    FoldKey, FoldState, InteractionKind, TuiAskUserBlock, TuiRenderUnit,
};
use ratatui_kit::prelude::*;

// ── entry 焦点导航纯函数（Slice 2 键盘语义层，视觉归 Slice 3）──────────────

/// 移动 entry 焦点：`Alt+Up`（delta=-1）/ `Alt+Down`（delta=+1）。
/// 无焦点时向上 → 最新 entry（末项），向下 → 首 entry；到达边界后钳制（不循环）。
pub(super) fn move_entry_focus(
    items_len: usize,
    current: Option<usize>,
    delta: i32,
) -> Option<usize> {
    if items_len == 0 {
        return None;
    }
    let base: i64 = match current {
        Some(i) => i as i64,
        None if delta < 0 => items_len as i64, // 从无焦点向上 → 最新 entry
        None => -1,                            // 从无焦点向下 → 首 entry 之前
    };
    let next = base + i64::from(delta);
    Some(next.clamp(0, items_len as i64 - 1) as usize)
}

/// entry 的折叠键 + 当前 fold（无折叠能力的 entry → `None`）。
/// 与折叠 pass（`acp_events/render.rs::apply_fold_pass`）的键控口径一致：
/// Reasoning(message_id) / Tool(tool_id) / SubAgent(agent_id) /
/// Interaction(request_id)。
pub(super) fn fold_key_of(vm: &TuiRenderUnit) -> Option<(FoldKey, FoldState)> {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(b) => {
            let r = b.reasoning.as_ref()?;
            Some((FoldKey::Reasoning(b.message_id.clone()?), r.fold))
        }
        TuiRenderUnit::TuiToolCard(t) => Some((FoldKey::Tool(t.tool_id.clone()), t.fold)),
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            Some((FoldKey::SubAgent(g.instance_id.clone()), g.fold))
        }
        TuiRenderUnit::TuiAskUserBlock(a) => {
            Some((FoldKey::Interaction(a.request_id.clone()?), a.fold))
        }
        _ => None,
    }
}

/// [S2 焦点单一事实源] 设 entry 焦点：一次写入 FOCUSED_ENTRY 完整表达导航
/// 事实（slot + key）。
/// [Why 锁内派生] 必须在持 VIEW_MODELS 写锁内调用——key 从锁内快照
/// items[slot] 派生：foldable entry 有值；无折叠能力 entry / request_id 缺失
/// 的 interaction 合法 `key: None`（slot 仍表达「焦点在消息区」）。桥线程
/// 可并发写 VIEW_MODELS，锁外读快照会读到漂移索引（key 与索引一致性由
/// 同一快照保证）。
pub(super) fn set_entry_focus(snapshot: &ViewModelsSnapshot, slot: usize) {
    let key = snapshot
        .items
        .get(slot)
        .and_then(fold_key_of)
        .map(|(k, _)| k);
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry { slot, key });
}

/// entry 单击结算判定（纯函数，S3 测试直调锁定）——单击 Up handler 中
/// 被消费前的唯一判定路径。
///
/// 单击 = Down 冻结 + 手势从未升级（`gesture` 保持 Pending 到 Up）：
/// `gesture` 为 Some 且冻结的 `entry_hit` 命中首行 `(slot, 0)` →
/// `Some(slot)`；否则 `None`（无 Down 记录 / 正文行 / 非首行）。
///
/// [防御 Up 坐标] `mouse_row` 必须在 `area` 内且非滚动条列——防御检查
/// 基于 Up 时点坐标，比 D2 设计表述（pending.screen）更严格（S1 review
/// L4），提取时保持该语义。
///
/// [D3 权衡] 不做 Down/Up 坐标比较：升级判定的唯一时机是 Drag 事件
/// （终端按住移动必发 Drag，Up 结算只看手势是否仍为 Pending）；无 Drag
/// 事件的超容差 Up（坐标差 10 行）仍判单击——有意识决策，S3 锁定用例。
pub(super) fn entry_click_decision(
    gesture: Option<&GesturePending>,
    mouse_row: u16,
    area: ratatui_kit::ratatui::layout::Rect,
    is_scrollbar_col: bool,
) -> Option<usize> {
    // 防御检查基于 Up 坐标：行越界 / 滚动条列不参与 entry 点击
    //（滚动条 Up 分支负责 thumb 释放）
    if mouse_row < area.y || mouse_row >= area.y.saturating_add(area.height) {
        return None;
    }
    if is_scrollbar_col {
        return None;
    }
    let pending = gesture?;
    match pending.entry_hit {
        Some((slot, 0)) => Some(slot),
        _ => None,
    }
}

/// 对 VM 应用手动折叠覆盖：写 fold + user_modified + 重算 hash（G1）。
/// 调用方必须先写 FOLD_OVERRIDES 覆盖表——快照重建（push_view_models）后
/// 由折叠 pass 依据覆盖表恢复 fold/user_modified，手动选择跨流式保持。
pub(super) fn apply_fold_override(vm: &mut TuiRenderUnit, fold: FoldState) {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(b) => {
            if let Some(r) = b.reasoning.as_mut() {
                r.fold = fold;
                b.recompute_hash();
            }
        }
        TuiRenderUnit::TuiToolCard(t) => {
            t.fold = fold;
            t.user_modified = true;
            t.recompute_hash();
        }
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            g.fold = fold;
            g.user_modified = true;
            g.recompute_hash();
        }
        TuiRenderUnit::TuiAskUserBlock(a) => {
            a.fold = fold;
            a.user_modified = true;
            a.recompute_hash();
        }
        _ => {}
    }
}

/// [Slice 4 §6.8] 取出 pending 的 interaction block（§6.8）——选项导航/提交
/// 的目标。completed（结果行）不在此列（走折叠切换）。
pub(super) fn pending_interaction_of(vm: &TuiRenderUnit) -> Option<&TuiAskUserBlock> {
    match vm {
        TuiRenderUnit::TuiAskUserBlock(a) if a.pending => Some(a),
        _ => None,
    }
}

/// [Slice 4 §6.8] interaction option 循环切换（Tab/← 后退、→ 前进；首末回绕）。
/// `count` 调用方已归一化 ≥1。后退在首项回绕到末项（循环语义——浏览器 Tab
/// 直觉；不能用 `saturating_sub`——首项会卡死无法回绕）。
pub(super) fn cycle_interaction_option(current: usize, count: usize, back: bool) -> usize {
    debug_assert!(count >= 1);
    if back {
        (current + count - 1) % count
    } else {
        (current + 1) % count
    }
}

/// [Slice 4 §6.8] 提交 interaction block 的指定选项（双轨 D5：与弹窗/面板
/// 同一响应通道——HITL_RESPONSE_TX / ASK_USER_RESPONSE_TX；InteractionTerminal
/// 结果回写由 ask_user_action / hitl_response 消费者发出）。同时关闭模态层
/// （HITL 弹窗 / AskUser 面板），保持双轨一致。request_id 缺失时 no-op。
pub(super) fn submit_interaction_option(block: &TuiAskUserBlock, option_index: usize) {
    submit_interaction_option_with(
        block,
        option_index,
        |action| {
            if let Some(tx) = crate::kit::atoms::HITL_RESPONSE_TX.get() {
                let _ = tx.send(action);
            }
        },
        |action| {
            if let Some(tx) = crate::kit::atoms::ASK_USER_RESPONSE_TX.get() {
                let _ = tx.send(action);
            }
        },
    );
}

pub(crate) fn submit_interaction_option_with(
    block: &TuiAskUserBlock,
    option_index: usize,
    mut send_hitl: impl FnMut(crate::kit::hitl_response::HitlResponseAction),
    mut send_ask_user: impl FnMut(crate::kit::ask_user_action::AskUserResponseAction),
) {
    let Some(id_str) = block.request_id.clone() else {
        return;
    };
    let Some(owner) = block.owner.clone() else {
        return;
    };
    match block.kind {
        InteractionKind::Permission => {
            // D6：HITL 只渲染 [Allow once] [Deny] 两选项（[Always allow] 为
            // 协议依赖项，记入 active spec）。
            let action = if option_index == 0 {
                crate::kit::hitl_response::HitlResponseAction::Approve {
                    owner: owner.clone(),
                    request_id_str: id_str.clone(),
                }
            } else {
                crate::kit::hitl_response::HitlResponseAction::Reject {
                    owner: owner.clone(),
                    request_id_str: id_str.clone(),
                }
            };
            send_hitl(action);
            crate::kit::popup_overlay::close_hitl_popup_for_owner(&owner);
        }
        InteractionKind::AskUser => {
            let label = block.options.get(option_index).cloned().unwrap_or_default();
            let answers = build_inline_answers(&block.question_ids, &label);
            send_ask_user(crate::kit::ask_user_action::AskUserResponseAction::Submit {
                owner: owner.clone(),
                request_id_str: id_str.clone(),
                answers,
            });
            crate::kit::panel_registry::close_ask_user_panel_for_owner(&owner);
        }
    }
}

/// [Slice 4 §6.8] AskUser inline 快速回答的 answers map：首问 = 选中 label，
/// 其余问题空字符串（协议结构完整——单选 string 类型，面板的空答案先例
/// `json!("")`）。question IDs 来自 durable block，不读取 active interaction。
pub(crate) fn build_inline_answers(question_ids: &[String], label: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (i, id) in question_ids.iter().enumerate() {
        let val = if i == 0 && !label.is_empty() {
            serde_json::json!(label)
        } else {
            serde_json::json!("")
        };
        map.insert(id.clone(), val);
    }
    serde_json::Value::Object(map)
}

/// [Slice 2/4] 对焦点 entry 应用折叠切换（Enter Collapsed↔Expanded /
/// Space → Preview）：写 FOLD_OVERRIDES + 当帧快照 COW set + 重算 hash。
/// 无折叠能力的 entry 消费但不动作（避免误触发送）。
/// 调用方保证 `idx < snapshot.items.len()`（焦点失效已在调用点处理）。
pub(super) fn apply_fold_toggle(
    snapshot: &mut ViewModelsSnapshot,
    idx: usize,
    next_is_preview: bool,
) -> EventResult {
    let Some((fold_key, current_fold)) = fold_key_of(&snapshot.items[idx]) else {
        // 无折叠能力的 entry（纯文本 assistant / user）：消费但不切换，
        // 避免误触发送。
        return EventResult::Consumed;
    };
    // [Slice 2] §6.7：subagent Enter → 打开详情 pane（不切折叠——subagent
    // 折叠恒 Collapsed 是 §7 表裁决，fold_key_of 不动）；Tool/Reasoning 的
    // Enter 语义不变。写 SELECTED_SUBAGENT_ID 供详情面板按 id 从 VIEW_MODELS
    // 扫描嵌套消息。
    if !next_is_preview && let FoldKey::SubAgent(agent_id) = &fold_key {
        *SELECTED_SUBAGENT_ID.state().write() = Some(agent_id.clone());
        crate::kit::panel_registry::open_panel(crate::app::panel_types::PanelKind::SubAgentDetail);
        return EventResult::Consumed;
    }
    let next = if next_is_preview {
        if current_fold == FoldState::Preview {
            FoldState::Collapsed
        } else {
            FoldState::Preview
        }
    } else if current_fold == FoldState::Collapsed {
        FoldState::Expanded
    } else {
        FoldState::Collapsed
    };
    // 持久覆盖表：快照重建（push_view_models）后由折叠 pass 恢复，
    // 手动选择跨流式/跨 turn 保持（spec §7）。
    FOLD_OVERRIDES.state().write().insert(fold_key, next);
    // 应用到当帧快照（COW set + 重算 hash）
    let mut updated = snapshot.items[idx].clone();
    apply_fold_override(&mut updated, next);
    snapshot.items.set(idx, updated);
    snapshot.generation = snapshot.generation.wrapping_add(1);
    EventResult::Consumed
}
