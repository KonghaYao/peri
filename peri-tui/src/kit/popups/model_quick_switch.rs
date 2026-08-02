//! ratatui-kit ModelQuickSwitchPopup component.
//!
//! 状态栏模型段（alias/model）点击弹出的 **小弹出层**——锚定在模型段上方，
//! 非居中大 modal：
//! - ↑/↓ 选择，Enter 切换并关闭（键盘全程可用，无需鼠标）；
//! - 鼠标 hover 行高亮（选中跟随悬停）；
//! - 鼠标点击行直接切换并关闭；
//! - 鼠标点击弹窗矩形之外关闭（dismiss-on-outside-click）；
//! - Esc 关闭（本组件消费，全局链兜底）。
//!
//! ## 定位
//!
//! `StatusBarRow1` 点击模型段时写入 `MODEL_SWITCH_ANCHOR`（模型段起点屏幕坐标），
//! 本组件读取后 `Positioned` 到锚点上方；上方空间不足时翻转到锚点下方。
//!
//! ## 行布局契约（hover/点击反推行号依赖）
//!
//! 渲染固定为：`lines[0..4]` 四档行（每行 `❯ alias model`），外加全边框上下各 1 行，
//! 无首尾空行。鼠标位置反推：`line_idx = row - (area.y + 1)`，`line_idx < ROW_COUNT`
//! 即对应档位——修改布局时必须同步更新 `ROW_COUNT` 与 `POPUP_HEIGHT`。

#![allow(clippy::needless_update)]

use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::Style,
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    },
};

use crate::kit::atoms::{
    LANG_VERSION, MODEL_SWITCH_ANCHOR, PERI_CONFIG_HANDLE, POPUP_KIND, PopupKind,
};
use crate::kit::list_nav::{ListNavAction, classify_list_nav, next_selection, previous_selection};
use crate::kit::mouse_router;
use crate::kit::panels::model::{PROFILE_KEYS, switch_active_alias};
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;
use unicode_width::UnicodeWidthStr;

/// 四档行数（与 PROFILE_KEYS 长度一致）
const ROW_COUNT: usize = 4;
/// 弹窗总高度：内容 4 行 + 全边框上下各 1 行
const POPUP_HEIGHT: u16 = 6;
/// 弹窗宽度下/上限
const POPUP_WIDTH_MIN: u16 = 36;
const POPUP_WIDTH_MAX: u16 = 80;

/// 弹窗单行数据——只显示 `alias model` 两段（用户要求精简，不含 provider/effort）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuickSwitchRow {
    pub alias: String,
    pub model: String,
}

/// 从配置构建四档行数据（纯函数，可测试）。
///
/// model 解析规则与 `panels/model.rs` 左侧卡片一致：Profile.model > provider.models
/// 同档位映射 > alias 名（provider 仅用于解析 model 名，不显示）。
fn quick_switch_rows(cfg: &crate::config::PeriConfig) -> Vec<QuickSwitchRow> {
    PROFILE_KEYS
        .iter()
        .map(|key| {
            let profile = cfg.config.profiles.get(key);
            let prov = profile.and_then(|pf| {
                if pf.provider.is_empty() {
                    cfg.config.providers.first()
                } else {
                    cfg.config.providers.iter().find(|p| p.id == pf.provider)
                }
            });
            let model = profile
                .and_then(|pf| pf.model.clone().filter(|m| !m.is_empty()))
                .or_else(|| {
                    prov.and_then(|p| p.models.get_model(key))
                        .map(str::to_string)
                })
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| key.to_string());
            QuickSwitchRow {
                alias: key.to_string(),
                model,
            }
        })
        .collect()
}

/// 弹窗宽度：按最宽行内容自适应（含 "❯" 前缀与 4 空格 padding），clamp 到合理范围。
fn popup_width(rows: &[QuickSwitchRow]) -> u16 {
    let max_content = rows
        .iter()
        .map(|r| format!(" {} {} {}", "❯", r.alias, r.model).as_str().width())
        .max()
        .unwrap_or(0);
    (max_content as u16 + 4).clamp(POPUP_WIDTH_MIN, POPUP_WIDTH_MAX)
}

/// 计算弹窗左上角屏幕坐标（纯函数，可测试）。
///
/// - x：对齐锚点 x（左侧留 2 列视觉缩进），超出右缘时 clamp 到 `term_w - w`；
/// - y：优先显示在锚点上方（gap 1 行）；上方空间不足时翻转到锚点下方。
fn position_at_anchor(ax: u16, ay: u16, w: u16, h: u16, term_w: u16, term_h: u16) -> (u16, u16) {
    let x = ax.saturating_add(2).min(term_w.saturating_sub(w));
    let y = if ay >= h.saturating_add(1) {
        // 上方空间充足：锚点上方，gap 1 行
        ay - h - 1
    } else {
        // 空间不足：翻转到锚点下方
        ay.saturating_add(2)
    };
    (x, y.min(term_h.saturating_sub(h)))
}

/// 鼠标位置 → 档位索引（纯函数，可测试）。
///
/// 区域外（含 top/bottom border）返回 None。
/// 行布局契约见文件头注释。
fn row_index_at(row: u16, col: u16, area: &Rect) -> Option<usize> {
    if row < area.y.saturating_add(1) || row >= area.y.saturating_add(area.height) {
        return None;
    }
    if col < area.x || col >= area.x.saturating_add(area.width) {
        return None;
    }
    let line_idx = (row - (area.y + 1)) as usize;
    if line_idx < ROW_COUNT {
        Some(line_idx)
    } else {
        None
    }
}

/// 点击位置是否在弹窗矩形内（含全边框）。
///
/// 与 `row_index_at` 的区别：内容行判定对 top/bottom border 返回 None，
/// 而 border 属于弹窗视觉范围——点击 border 不关闭弹窗，仅矩形外触发 dismiss。
fn click_inside_popup(row: u16, col: u16, area: &Rect) -> bool {
    row >= area.y
        && row < area.y.saturating_add(area.height)
        && col >= area.x
        && col < area.x.saturating_add(area.width)
}

#[component]
pub fn ModelQuickSwitchPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    let anchor_store = hooks.use_atom(&MODEL_SWITCH_ANCHOR);
    let (term_w, term_h) = hooks.use_terminal_size();

    // 当前 active alias（配置唯一事实源；弹窗每次打开都是新实例，初始值正确）
    let active_alias = PERI_CONFIG_HANDLE
        .get()
        .map(|h| h.read().config.active_alias.clone())
        .unwrap_or_else(|| "opus".to_string());
    let initial_sel = PROFILE_KEYS
        .iter()
        .position(|k| *k == active_alias)
        .unwrap_or(1);
    let sel = hooks.use_state(|| initial_sel);

    // 弹窗几何（每帧按当前 anchor/终端尺寸重算，闭包按值捕获副本）
    let cfg = PERI_CONFIG_HANDLE.get().map(|h| h.read().clone());
    let rows = cfg.as_ref().map(quick_switch_rows).unwrap_or_default();
    let width = popup_width(&rows);
    let height = POPUP_HEIGHT;
    let (x, y) = match *anchor_store.read() {
        Some((ax, ay)) => position_at_anchor(ax, ay, width, height, term_w, term_h),
        // 防御：锚点缺失（异常路径）时渲染空，避免占位弹窗
        None => {
            return element!(Positioned(x: 0u16, y: 0u16, width: 0u16, height: 0u16, clear: false));
        }
    };
    let area = Rect::new(x, y, width, height);

    // ── 键盘导航（Current scope：弹窗激活期间才收事件）──
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            match classify_list_nav(&key) {
                Some(ListNavAction::MoveUp) => {
                    let mut s = sel.write();
                    *s = previous_selection(*s);
                    return EventResult::Consumed;
                }
                Some(ListNavAction::MoveDown) => {
                    let mut s = sel.write();
                    *s = next_selection(*s, PROFILE_KEYS.len());
                    return EventResult::Consumed;
                }
                Some(ListNavAction::Confirm) => {
                    switch_active_alias(*sel.read());
                    close_popup();
                    return EventResult::Consumed;
                }
                Some(ListNavAction::Cancel) => {
                    close_popup();
                    return EventResult::Consumed;
                }
                // Tab 切换视图在本弹窗无意义——不消费（留给外层）
                Some(ListNavAction::CycleForward | ListNavAction::CycleBackward) => {}
                None => {}
            }
        }
        EventResult::Ignored
    });

    // ── 鼠标 hover + 点击（Global scope，确保弹窗打开时事件可达）──
    // hover 仅在行号变化时写选中（避免 Moved 高频事件触发无谓重渲染，
    // 见 spec/archive-issues/tui-message-area/2026-07-05-mouse-move-cpu-spike）。
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        if let Event::Mouse(mouse) = event {
            // 被其他前景层遮挡时让路（防御性：遮挡判定集中见 kit/mouse_router.rs）。
            // 自身不算遮挡——POPUP_KIND 是单值，本弹窗渲染期间恒为自己的 kind，
            // 直接 is_occluded() 会恒真导致自身鼠标功能失效。
            if mouse_router::is_occluded()
                && !matches!(
                    POPUP_KIND.state().read().as_ref(),
                    Some(PopupKind::ModelQuickSwitch)
                )
            {
                return EventResult::Ignored;
            }
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(idx) = row_index_at(mouse.row, mouse.column, &area) {
                        let mut s = sel.write();
                        if *s != idx {
                            *s = idx;
                        }
                        return EventResult::Consumed;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(idx) = row_index_at(mouse.row, mouse.column, &area) {
                        switch_active_alias(idx);
                        close_popup();
                        return EventResult::Consumed;
                    }
                    // 点击弹窗矩形（含边框）之外 → 关闭弹窗（dismiss-on-outside-click）。
                    // 矩形内非内容行（边框/padding）点击不关闭也不切换，Consumed 防止
                    // 事件穿透到背景组件。
                    if !click_inside_popup(mouse.row, mouse.column, &area) {
                        close_popup();
                    }
                    return EventResult::Consumed;
                }
                _ => {}
            }
        }
        EventResult::Ignored
    });

    // ── 渲染 ──
    let guard = theme_def.read();
    let popup_tokens = &guard.component.popup;
    let semantic = &guard.semantic;
    let cur_sel = *sel.read();

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let is_sel = i == cur_sel;
        let is_active = row.alias == active_alias;
        let prefix = if is_sel {
            "❯"
        } else if is_active {
            "●"
        } else {
            "○"
        };
        // 只显示 `alias model` 两段（用户要求精简）
        let alias_span = format!(" {} {}", prefix, row.alias);
        let model_text = truncate_str(&row.model, 36);
        if is_sel {
            lines.push(Line::from(vec![
                Span::styled(alias_span, Style::new().fg(semantic.accent).bold()),
                Span::styled(
                    format!(" {model_text}"),
                    Style::new().fg(semantic.accent).bold(),
                ),
            ]));
        } else {
            let alias_style = if is_active {
                Style::new().fg(semantic.status.success).bold()
            } else {
                Style::new().fg(semantic.text.primary)
            };
            lines.push(Line::from(vec![
                Span::styled(alias_span, alias_style),
                Span::styled(
                    format!(" {model_text}"),
                    Style::new().fg(semantic.text.muted),
                ),
            ]));
        }
    }

    let popup_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(popup_tokens.border));
    let text_render = Paragraph::new(ratatui::text::Text::from(lines)).block(popup_block);

    element!(
        Positioned(x: x, y: y, width: width, height: height, clear: true) {
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: text_render)
            }
        }
    )
}

/// 按显示宽度截断字符串，超长加省略号（输出总宽度 ≤ max_width）。
fn truncate_str(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        s.to_string()
    } else {
        let mut out = String::new();
        let mut w = 0usize;
        for ch in s.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > max_width.saturating_sub(1) {
                break;
            }
            out.push(ch);
            w += cw;
        }
        format!("{out}…")
    }
}

#[cfg(test)]
#[path = "model_quick_switch_test.rs"]
mod tests;
