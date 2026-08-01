//! ratatui-kit RewindPopup component.
//!
//! 回退弹窗（Rewind v2）三态：
//! - Candidates：候选列表（`REWIND_PREVIEW.messages`，只 user 消息）——
//!   Up/Down 选择、Enter 发送 `RewindAction::Preview`（暂存目标文本 + 预算查询）
//! - Budget：文件回退预算（`REWIND_BUDGET_STATE = Files(v)`）——
//!   Enter 发送 `RewindAction::Confirm` 执行回退、Esc 返回候选视图
//! - Executing：正在回退（`REWIND_BUDGET_STATE = Executing`）——
//!   等待 RewindCompleted 事件关闭
//!
//! ## 数据源
//!
//! 候选由 `kit/rewind_candidates.rs::spawn_candidates_query` 在双击 Esc 时
//! 实时查询 `session/rewind-candidates` 写入 `REWIND_PREVIEW` atom；
//! 预算/执行中状态由 `kit/rewind_action.rs` consumer 写入
//! `REWIND_BUDGET_STATE`；查询失败写 `REWIND_QUERY_ERROR`。

#![allow(clippy::needless_update)]

use peri_acp_types::event_data::RewindPreview;
use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{style::Stylize, text::Line},
};

use crate::i18n;
use crate::kit::atoms::{
    LANG_VERSION, RENDER_HEARTBEAT, REWIND_ACTION_TX, REWIND_BUDGET_STATE, REWIND_QUERY_ERROR,
    REWIND_TARGET_TEXT, RewindBudgetState,
};
use crate::kit::list_nav::{ListNavAction, classify_list_nav, next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item};
use crate::kit::popup_overlay::close_popup;
use crate::kit::rewind_action::RewindAction;

/// 弹窗视图——由 `REWIND_BUDGET_STATE` 推导，非用户可切换状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewindView {
    Candidates,
    Budget,
    Executing,
}

#[component]
pub fn RewindPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let preview_store = hooks.use_atom(&crate::kit::atoms::REWIND_PREVIEW);
    let preview = preview_store.read().clone();
    let budget_store = hooks.use_atom(&crate::kit::atoms::REWIND_BUDGET_STATE);
    let budget_state = budget_store.read().clone();
    let query_error = hooks
        .use_atom(&crate::kit::atoms::REWIND_QUERY_ERROR)
        .read()
        .clone();

    hooks.use_atom(&LANG_VERSION);

    // 视图推导：预算 Files 非空 → Budget；Executing → Executing；否则 Candidates
    let view = match &budget_state {
        RewindBudgetState::Files(v) if !v.is_empty() => RewindView::Budget,
        RewindBudgetState::Executing => RewindView::Executing,
        _ => RewindView::Candidates,
    };
    // 候选选中索引（最新一条 = 回退一步）
    let msg_sel = hooks.use_state(|| 0usize);
    // 预算视图选中索引（默认最新变更 = 第一条）
    let file_sel = hooks.use_state(|| 0usize);

    let msg_count = preview.as_ref().map(|p| p.messages.len()).unwrap_or(0);

    // 弹窗绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 行布局（候选视图：空行、标题、消息项（take(8)）、超量省略行、空行、提示行）
    let msg_rendered = msg_count.min(8);
    let msg_extra = usize::from(msg_count > 8);
    let lines_len = 1 + 1 + msg_rendered + msg_extra + 1 + 1;
    let msg_layout = ListLayout {
        header_rows: 2,
        item_rows: 1,
        footer_rows: lines_len.saturating_sub(2 + msg_rendered + msg_extra) as u16,
        visible_items: msg_rendered as u16,
        scroll_start: 0,
        item_count: msg_count,
    };

    // 闭包另持一份 preview 副本（避免与渲染端争用 move）
    let preview_for_closure = preview.clone();

    // 渲染行构造（纯函数，测试友好）
    let lines: Vec<Line<'static>> = build_popup_lines(
        &preview,
        &budget_state,
        &query_error,
        *msg_sel.read(),
        *file_sel.read(),
        &theme_def.read(),
    );

    // 事件处理：Candidates 视图处理 Up/Down/Enter + 鼠标点击；
    // Budget 视图处理 Enter（确认）/ Esc（返回）；Esc 在 Candidates 关闭弹窗。
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High, // P1：根层 Esc 为 Normal（event_handlers.rs:149），
        // 同优先级下根层先注册先消费——弹窗内 Esc 分支变死代码。
        // 改 High 后弹窗自管理 Esc（关闭/返回候选）。
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：候选区左键点击 = 选中 + 发送 Preview（与键盘 Enter 一致）
            if let Event::Mouse(mouse) = event {
                if let (Some(area), true) = (area, view == RewindView::Candidates)
                    && let Some(idx) = hit_item(&mouse, area, msg_layout)
                {
                    *msg_sel.write() = idx;
                    let target = preview_for_closure
                        .as_ref()
                        .and_then(|p| p.messages.get(idx));
                    if let Some(m) = target
                        && let Some(tx) = REWIND_ACTION_TX.get()
                    {
                        let _ = tx.send(RewindAction::Preview {
                            target_message_id: m.id.clone(),
                            target_text: m.preview.clone(),
                        });
                    }
                    return EventResult::Consumed;
                }
                // 区域内的左键点击（未命中行）也消费，防止穿透到消息区
                return match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                    _ => EventResult::Ignored,
                };
            }
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                match classify_list_nav(&key) {
                    Some(ListNavAction::MoveUp) if view == RewindView::Candidates => {
                        let mut s = msg_sel.write();
                        *s = previous_selection(*s);
                        return EventResult::Consumed;
                    }
                    Some(ListNavAction::MoveDown) if view == RewindView::Candidates => {
                        let mut s = msg_sel.write();
                        *s = next_selection(*s, msg_count);
                        return EventResult::Consumed;
                    }
                    Some(ListNavAction::Confirm) => match view {
                        RewindView::Candidates => {
                            let target = preview_for_closure
                                .as_ref()
                                .and_then(|p| p.messages.get(*msg_sel.read()));
                            if let Some(m) = target
                                && let Some(tx) = REWIND_ACTION_TX.get()
                            {
                                let _ = tx.send(RewindAction::Preview {
                                    target_message_id: m.id.clone(),
                                    target_text: m.preview.clone(),
                                });
                            }
                            // 弹窗保持打开：等待预算/执行结果
                            return EventResult::Consumed;
                        }
                        RewindView::Budget => {
                            let target = preview_for_closure
                                .as_ref()
                                .and_then(|p| p.messages.get(*msg_sel.read()));
                            if let Some(m) = target
                                && let Some(tx) = REWIND_ACTION_TX.get()
                            {
                                let _ = tx.send(RewindAction::Confirm {
                                    target_message_id: m.id.clone(),
                                });
                            }
                            // 保持打开：等待执行完成
                            return EventResult::Consumed;
                        }
                        RewindView::Executing => {
                            return EventResult::Consumed;
                        }
                    },
                    Some(ListNavAction::Cancel) => match view {
                        RewindView::Executing => {
                            // P1：执行中态 Esc——RPC 已发出、服务端正在回退，
                            // RewindCompleted 必达。保留 REWIND_TARGET_TEXT 等待
                            // 回填；仅回候选视图。若 RPC 失败，rewind_consumer
                            // 失败路径会清目标文本并显示错误。
                            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                            return EventResult::Consumed;
                        }
                        RewindView::Budget => {
                            // 预算确认前 Esc：尚未执行，目标文本不再需要
                            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                            *REWIND_TARGET_TEXT.state().write() = None;
                            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                            return EventResult::Consumed;
                        }
                        RewindView::Candidates => {
                            close_popup();
                            *REWIND_TARGET_TEXT.state().write() = None;
                            *REWIND_BUDGET_STATE.state().write() = RewindBudgetState::Idle;
                            *REWIND_QUERY_ERROR.state().write() = None;
                            return EventResult::Consumed;
                        }
                    },
                    _ => {}
                }
            }
            EventResult::Ignored
        },
    );

    let guard = theme_def.read();
    let semantic = &guard.semantic;

    popup_text_shell!(i18n::tr("rewind-title"), semantic.status.warning, lines)
}

/// 构造弹窗内容行（纯函数，测试友好）。
fn build_popup_lines(
    preview: &Option<RewindPreview>,
    budget_state: &RewindBudgetState,
    query_error: &Option<String>,
    msg_sel: usize,
    file_sel: usize,
    theme: &ThemeDefinition,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));
    let semantic = &theme.semantic;

    match budget_state {
        RewindBudgetState::Executing => {
            lines.push(Line::from("  正在回退…").fg(semantic.text.primary));
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
            return lines;
        }
        RewindBudgetState::Files(files) => {
            lines.push(
                Line::from(format!("  回退将撤销 {} 个文件改动：", files.len()))
                    .fg(semantic.text.primary)
                    .bold(),
            );
            for (i, fc) in files.iter().enumerate().take(8) {
                let is_selected = i == file_sel;
                let prefix = if is_selected { "  > " } else { "    " };
                let kind_label = if fc.kind == "write" { "write" } else { "edit" };
                lines.push(
                    Line::from(format!(
                        "{prefix}[{kind_label}] {}",
                        truncate_str(&fc.path, 40)
                    ))
                    .fg(if is_selected {
                        theme.component.popup.selected_fg
                    } else {
                        semantic.text.primary
                    }),
                );
            }
            if files.len() > 8 {
                lines.push(
                    Line::from(format!("    ... and {} more", files.len() - 8))
                        .fg(semantic.text.dim),
                );
            }
            lines.push(Line::from(""));
            lines.push(Line::from("  Enter 确认回退 · Esc 返回候选").fg(semantic.text.dim));
            return lines;
        }
        RewindBudgetState::Idle => {}
    }

    // ── Candidates 视图 ──
    if let Some(err) = query_error {
        lines.push(Line::from(format!("  查询失败: {err}")).fg(semantic.status.error));
        lines.push(Line::from(""));
        lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        return lines;
    }
    let Some(p) = preview else {
        lines.push(
            Line::from("  正在加载回退候选…")
                .fg(semantic.text.muted)
                .italic(),
        );
        lines.push(Line::from(""));
        lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        return lines;
    };
    if p.messages.is_empty() {
        lines.push(Line::from("  无可回退的消息。").fg(semantic.text.dim));
        lines.push(Line::from("  完成一轮对话后双击 Esc 即可回滚。").fg(semantic.text.dim));
        lines.push(Line::from(""));
        lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        return lines;
    }
    lines.push(
        Line::from(format!("  回退到（{}）", p.messages.len()))
            .fg(semantic.text.primary)
            .bold(),
    );
    for (i, msg) in p.messages.iter().enumerate().take(8) {
        let is_selected = i == msg_sel;
        let prefix = if is_selected { "  > " } else { "    " };
        lines.push(
            Line::from(format!("{prefix}{}", truncate_str(&msg.preview, 40))).fg(if is_selected {
                theme.component.popup.selected_fg
            } else {
                semantic.text.primary
            }),
        );
    }
    if p.messages.len() > 8 {
        lines.push(
            Line::from(format!("    ... and {} more", p.messages.len() - 8)).fg(semantic.text.dim),
        );
    }
    lines.push(Line::from(""));
    lines.push(Line::from("  Enter 回退 · Esc 关闭").fg(semantic.text.dim));
    lines
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────

/// 截断字符串到 max chars（CJK 安全）。
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}…", truncated)
    }
}

#[cfg(test)]
#[path = "rewind_popup_test.rs"]
mod tests;
