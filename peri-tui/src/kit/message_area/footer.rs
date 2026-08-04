use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::components::spinner::verb;
use crate::components::spinner::{SpinnerMode, SpinnerState};
use crate::i18n;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Color, Modifier, Style};
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

/// idle 态静止图标行：固定第一帧（不参与动画），muted 色，附带默认 verb。
///
/// spinner 组件常驻——active 时动画转动，inactive 时以此占位，永不 hidden。
/// [Why 固定帧] render_to_lines 的帧由壁钟纯计算（Idle 态也会转），
/// inactive 应静止，故单独输出 `tick_to_frame(0)` 而非复用动画路径。
/// [Why 带 verb] 历史会话恢复等场景没有耗时 summary，若只显示图标会显得
/// 突兀（只有符号没有文字），附带一句默认成语占位，与 loading 行视觉一致。
pub(super) fn render_idle_spinner_line(color: Color, verb: &str) -> Line<'static> {
    let frame = crate::components::spinner::animation::tick_to_frame(0);
    Line::from(vec![
        Span::styled(format!("{frame} "), Style::default().fg(color)),
        Span::styled(verb.to_string(), Style::default().fg(color)),
    ])
}

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
/// 返回 `(lines, layout, has_content)`：layout 仅在按钮实际渲染时存在；
/// `has_content` 表示 footer 是否有实质内容（loading / summary / todo），
/// 供调用方区分"footer 常驻占位"与"真实内容"（如 Welcome 空态判定）。
pub(super) fn build_footer_lines(
    hooks: &mut Hooks,
    is_loading: bool,
    todo_items: &[TodoItem],
    keepgoing_blocked: bool,
    vis_width: u16,
) -> (Vec<Line<'static>>, Option<KeepGoingLayout>, bool) {
    let semantic = THEME_ATOM.state().read().semantic;

    let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));
    let load_start = hooks.use_state(|| Option::<Instant>::None);
    let was_loading = hooks.use_state(|| false);
    let summary_elapsed_ms = hooks.use_state(|| 0u64);
    let loading_epoch = hooks.use_atom(&LOADING_EPOCH);
    let last_epoch = hooks.use_state(|| 0u64);
    // idle 占位行的默认 verb：会话期间固定一句成语，避免每次渲染随机闪变。
    let idle_verb = hooks.use_state(|| verb::pick_verb(None));

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
    let has_content = is_loading || has_summary || !todo_items.is_empty();

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut keepgoing_layout: Option<KeepGoingLayout> = None;
    // footer 常驻：spinner 组件永远渲染在消息区下方（active 动画 / summary / idle 静止图标）。
    // [Why] 无早退——恢复历史（rewind/session new）会清零 summary，若此时 idle 且无 todo，
    // 旧实现提前返回空导致 spinner 组件整体消失（hidden），违背 active/inactive 二态约定。
    lines.push(Line::from(""));
    lines.push(Line::from(""));
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
        // 样式与 md 复制按钮统一：左右各 1 空格 + 反色（REVERSED）。
        // 防抖期间以 muted 色渲染（不可点击）。
        let btn_span = Span::styled(
            format!(" {btn_text} "),
            Style::default()
                .fg(if keepgoing_blocked {
                    semantic.text.muted
                } else {
                    semantic.accent
                })
                .add_modifier(Modifier::REVERSED),
        );
        let btn_width = btn_span.width() as u16;
        // 按钮与 summary 文本之间空 1 个普通空格（不反色）表示间距，
        // 按钮点击区域从该空格之后算起。
        let start_col = summary_line.width() as u16 + 1;
        // [Fix m4] 窄终端下 summary + 按钮超宽时 WordWrapper 会把按钮换到下一
        // 视觉行，而 compute_keepgoing_rect 按"每 footer 行占 1 视觉行"假设计算
        // 点击区域——换行后按钮实际位置与 rect 错位、点击失效。超宽时跳过按钮
        // 渲染（布局保持单行，rect 不产生）。
        if start_col.saturating_add(btn_width) <= vis_width {
            keepgoing_layout = Some(KeepGoingLayout {
                line_index: lines.len(),
                start_col,
                width: btn_width,
            });
            let mut line = summary_line;
            line.spans.push(Span::raw(" "));
            line.spans.push(btn_span);
            lines.push(line);
        } else {
            tracing::debug!(
                start_col,
                btn_width,
                vis_width,
                "keepgoing: footer line exceeds vis_width, button hidden"
            );
            lines.push(summary_line);
        }
    }
    if !todo_items.is_empty() {
        lines.extend(render_todo_lines(todo_items));
    } else if !is_loading && !has_summary {
        // idle：静止图标 + 默认 verb 占位（inactive 态）。todo/summary 存在时本身即为占位内容。
        lines.push(render_idle_spinner_line(
            semantic.text.muted,
            &idle_verb.read(),
        ));
    }
    lines.push(Line::from(""));
    (lines, keepgoing_layout, has_content)
}

#[cfg(test)]
#[path = "footer_test.rs"]
mod tests;
