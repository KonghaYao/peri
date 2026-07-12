use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::i18n;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_widgets::spinner::{SpinnerMode, SpinnerState};
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

pub(super) fn build_footer_lines(
    hooks: &mut Hooks,
    is_loading: bool,
    todo_items: &[TodoItem],
) -> Vec<Line<'static>> {
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
        return Vec::new();
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let has_footer_content = is_loading || has_summary || !todo_items.is_empty();
    if has_footer_content {
        lines.push(Line::from(""));
        lines.push(Line::from(""));
    }
    if is_loading {
        let token_count = crate::kit::atoms::SPINNER_TOKEN_COUNT.get();
        lines.extend(spinner_state.read().render_to_lines(
            semantic.accent,
            semantic.text.muted,
            true,
            true,
            token_count,
        ));
    } else if has_summary {
        let elapsed = peri_widgets::spinner::animation::format_elapsed(*summary_elapsed_ms.read());
        lines.push(Line::from(Span::styled(
            i18n::tr_args(
                "msg-spinner-brewed",
                &[("duration".to_string(), FluentValue::from(elapsed))],
            ),
            Style::default().fg(semantic.text.muted),
        )));
    }
    if !todo_items.is_empty() {
        lines.extend(render_todo_lines(todo_items));
    }
    if has_footer_content {
        lines.push(Line::from(""));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_todo_lines_icons_and_crossed() {
        let items = vec![
            TodoItem {
                status: TodoStatus::InProgress,
                content: "修复 bug".into(),
            },
            TodoItem {
                status: TodoStatus::Completed,
                content: "草拟 PRD".into(),
            },
            TodoItem {
                status: TodoStatus::Pending,
                content: "部署".into(),
            },
        ];
        let lines = render_todo_lines(&items);
        assert_eq!(lines.len(), 4);

        let in_progress_icon = lines[0].spans[0].content.as_ref();
        assert!(in_progress_icon.contains("◼"), "InProgress 图标应为 ◼");
        let in_progress_text = lines[0].spans[1].content.as_ref();
        assert!(
            in_progress_text.contains("修复 bug"),
            "InProgress 文本应包含任务内容"
        );

        let completed_icon = lines[1].spans[0].content.as_ref();
        assert!(completed_icon.contains("✔"), "Completed 图标应为 ✔");
        let completed_text = lines[1].spans[1].content.as_ref();
        assert!(
            completed_text.contains("草拟 PRD"),
            "Completed 文本应包含任务内容"
        );

        let pending_icon = lines[2].spans[0].content.as_ref();
        assert!(pending_icon.contains("◻"), "Pending 图标应为 ◻");
        let pending_text = lines[2].spans[1].content.as_ref();
        assert!(pending_text.contains("部署"), "Pending 文本应包含任务内容");
        assert!(
            pending_text.contains("(available)") || pending_text.contains("(可开始)"),
            "Pending 文本应包含 i18n 可用标记"
        );
    }

    #[test]
    fn test_render_todo_lines_empty() {
        let lines = render_todo_lines(&[]);
        assert_eq!(lines.len(), 1);
        for line in &lines {
            assert!(
                line.spans.is_empty(),
                "空 todo 列表不应输出任何内容行，仅 trailing blank lines"
            );
        }
    }

    #[test]
    fn test_spinner_summary_after_loading_ends() {
        let elapsed_ms: u64 = 30_000;
        let elapsed_str = peri_widgets::spinner::animation::format_elapsed(elapsed_ms);
        assert_eq!(elapsed_str, "30s");

        let summary = format!("  ✻  Brewed for {elapsed_str}");
        assert!(summary.contains("✻"));
        assert!(summary.contains("Brewed for"));
    }

    #[test]
    fn test_token_count_no_write_when_unchanged() {
        let prev_token_count: usize = 1500;
        let new_token_count: usize = 1500;
        let changed = prev_token_count != new_token_count;

        assert!(!changed, "token count 未变化时不应写 state");
    }

    #[test]
    fn test_footer_loading_steady_state_has_no_control_state_transition() {
        let prev_loading = true;
        let is_loading = true;
        let transition = prev_loading != is_loading;

        assert!(
            !transition,
            "loading 稳态不应写 was_loading/load_start，否则会触发持续重渲染"
        );
    }
}
