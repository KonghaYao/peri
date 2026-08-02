use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::components::spinner::{SpinnerMode, SpinnerState};
use crate::i18n;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};

use crate::kit::atoms::LOADING_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TodoStatus {
    InProgress,
    Completed,
    Pending,
}

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub status: TodoStatus,
    pub content: String,
}

pub(super) fn hash_todo_items(items: &[TodoItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.status.hash(&mut hasher);
        item.content.hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn render_todo_lines(items: &[TodoItem]) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = Vec::new();
    for item in items {
        let (icon, icon_color, text_color, crossed) = match item.status {
            TodoStatus::InProgress => ("◼", sem.accent, sem.text.primary, false),
            TodoStatus::Completed => ("✔", sem.status.success, sem.text.muted, true),
            TodoStatus::Pending => ("◻", sem.text.muted, sem.text.muted, false),
        };
        let prefix_style = Style::default().fg(icon_color).add_modifier(Modifier::BOLD);
        let mut text_style = Style::default().fg(text_color);
        if crossed {
            text_style = text_style.add_modifier(Modifier::CROSSED_OUT);
        }
        let prefix = Span::styled(format!("  {}  ", icon), prefix_style);
        let mut content = item.content.clone();
        if item.status == TodoStatus::Pending {
            content.push_str(&i18n::tr("msg-todo-available"));
        }
        let text = Span::styled(content, text_style);
        lines.push(Line::from(vec![prefix, text]));
    }
    for _ in 0..1 {
        lines.push(Line::from(""));
    }
    lines
}

// ── footer 行构建 ─────────────────────────────────────────────────────────

/// keepgoing 按钮在 footer 行内的布局信息（供 MessageArea 计算屏幕点击区域）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeepGoingLayout {
    /// 按钮所在行在 footer_lines 中的索引（spinner/summary 行）。
    pub(super) line_index: usize,
    /// 按钮起始列（行内，按显示宽度计）。
    pub(super) start_col: u16,
    /// 按钮显示宽度（列）。
    pub(super) width: u16,
}

/// 构建 footer 行 + keepgoing 按钮布局信息。
///
/// `keepgoing_blocked` 为 true 时按钮以禁用样式渲染（防抖中，不可点击）。
/// 返回 `(lines, Some(layout))`：layout 仅在按钮实际渲染时存在。
pub(super) fn build_footer_lines(
    hooks: &mut Hooks,
    is_loading: bool,
    todo_items: &[TodoItem],
    keepgoing_blocked: bool,
) -> (Vec<Line<'static>>, Option<KeepGoingLayout>) {
    let semantic = THEME_ATOM.state().read().semantic;

    let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));
    let load_start = hooks.use_state(|| Option::<Instant>::None);
    let was_loading = hooks.use_state(|| false);
    let summary_elapsed_ms = hooks.use_state(|| 0u64);
    let loading_epoch = hooks.use_atom(&LOADING_EPOCH);
    let last_epoch = hooks.use_state(|| 0u64);

    let last_reset_counter = hooks.use_state(|| crate::kit::atoms::BRIDGE_RESET_COUNTER.get());
    {
        let current = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
        // [TRAP] read guard 必须 drop 后再 write——同线程 parking_lot::RwLock read+write 会死锁
        // （deadlock_detection 默认关闭，静默卡死）。先把 read 值 copy 到 owned。
        let prev_counter = *last_reset_counter.read();
        if prev_counter != current {
            *summary_elapsed_ms.write() = 0;
            *last_reset_counter.write() = current;
        }
    }

    {
        let current_epoch = *loading_epoch.read();
        // [TRAP] 同上：read+write 同一 state 不可并存
        let prev_epoch = *last_epoch.read();
        if is_loading && prev_epoch != current_epoch {
            *last_epoch.write() = current_epoch;
            *load_start.write() = Some(Instant::now());
            *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
            *was_loading.write() = true;
        }

        let prev_loading = *was_loading.read();
        if prev_loading != is_loading {
            let mut ls = load_start.write();
            if is_loading {
                if ls.is_none() {
                    *ls = Some(Instant::now());
                    *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
                }
            } else {
                *summary_elapsed_ms.write() =
                    ls.map_or(0, |start| start.elapsed().as_millis() as u64);
                *ls = None;
            }
            *was_loading.write() = is_loading;
        }
    }

    let has_summary = *summary_elapsed_ms.read() > 0;
    if !is_loading && todo_items.is_empty() && !has_summary {
        return (Vec::new(), None);
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut keepgoing_layout: Option<KeepGoingLayout> = None;
    let has_footer_content = is_loading || has_summary || !todo_items.is_empty();
    if has_footer_content {
        lines.push(Line::from(""));
        lines.push(Line::from(""));
    }
    if is_loading {
        let token_count = crate::kit::atoms::SPINNER_TOKEN_COUNT.get();
        let summary = crate::kit::atoms::PREDICTION.state().read().summary.clone();
        lines.extend(spinner_state.read().render_to_lines(
            semantic.accent,
            semantic.text.muted,
            true,
            true,
            token_count,
            summary.as_deref(),
        ));
    } else if has_summary {
        let elapsed =
            crate::components::spinner::animation::format_elapsed(*summary_elapsed_ms.read());
        let summary_text = i18n::tr_args(
            "msg-spinner-brewed",
            &[("duration".to_string(), FluentValue::from(elapsed))],
        );
        let summary_line = Line::from(Span::styled(
            summary_text,
            Style::default().fg(semantic.text.muted),
        ));
        let btn_text = i18n::tr("msg-keepgoing");
        // keepgoing 按钮：仅 agent 空闲（spinner 不转）时追加到 summary 行右侧。
        // 防抖期间以 muted 样式渲染（不可点击）。
        let btn_span = Span::styled(
            format!(" {btn_text}"),
            Style::default()
                .fg(if keepgoing_blocked {
                    semantic.text.muted
                } else {
                    semantic.accent
                })
                .add_modifier(Modifier::BOLD),
        );
        let btn_width = btn_span.width() as u16;
        let start_col = summary_line.width() as u16;
        keepgoing_layout = Some(KeepGoingLayout {
            line_index: lines.len(),
            start_col,
            width: btn_width,
        });
        let mut line = summary_line;
        line.spans.push(btn_span);
        lines.push(line);
    }
    if !todo_items.is_empty() {
        lines.extend(render_todo_lines(todo_items));
    }
    if has_footer_content {
        lines.push(Line::from(""));
    }
    (lines, keepgoing_layout)
}

#[cfg(test)]
#[path = "footer_test.rs"]
mod tests;
