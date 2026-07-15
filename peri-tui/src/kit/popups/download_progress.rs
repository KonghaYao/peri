//! ratatui-kit DownloadProgressPopup component.
//!
//! 下载进度弹窗：展示从 GitHub 下载主题文件的进度。
//! - 下载中：逐文件显示状态（Pending → Downloading → Done/Failed）
//! - 下载完成后 Esc 关闭；下载中 Esc 无效（防止误关闭）

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        style::Style,
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::i18n;
use crate::kit::atoms::{DOWNLOAD_PROGRESS, FileDownloadStatus, LANG_VERSION};
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;

/// 弹窗最大行数（标题 + 条目）
const MAX_VISIBLE_ITEMS: usize = 16;

#[component]
pub fn DownloadProgressPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let progress_store = hooks.use_atom(&DOWNLOAD_PROGRESS);
    let payload = progress_store.read().clone();
    let finished = payload.finished;
    let _ = progress_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        match (key.modifiers, key.code) {
            // Esc 仅在下载完成后可关闭；下载中忽略 Esc（防止误关闭）
            (KeyModifiers::NONE, KeyCode::Esc) => {
                let dl = DOWNLOAD_PROGRESS.state();
                let current = dl.read().clone();
                if current.finished {
                    close_popup();
                    return EventResult::Consumed;
                }
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    });

    let guard = theme_def.read();
    let title_fg = guard.semantic.text.primary;
    let muted_style = Style::new().fg(guard.semantic.text.muted);
    let success_style = Style::new().fg(guard.semantic.status.success);
    let error_style = Style::new().fg(guard.semantic.status.error);
    let active_style = Style::new().fg(guard.semantic.accent);
    let dim_style = Style::new().fg(guard.semantic.text.dim);
    drop(guard);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // 进度概览（用于弹窗标题栏）
    let total = payload.items.len();
    let done_count = payload.success_count;
    let failed_count = payload.fail_count;
    let title_style = Style::new().fg(title_fg).bold();
    let title_line = if finished {
        Line::from(
            i18n::tr_args(
                "popup-download-title-done",
                &[
                    (
                        "total".to_string(),
                        fluent_bundle::FluentValue::from(total as i64),
                    ),
                    (
                        "success".to_string(),
                        fluent_bundle::FluentValue::from(done_count as i64),
                    ),
                    (
                        "failed".to_string(),
                        fluent_bundle::FluentValue::from(failed_count as i64),
                    ),
                ],
            )
            .to_string(),
        )
        .style(title_style)
        .centered()
    } else {
        Line::from(
            i18n::tr_args(
                "popup-download-title-active",
                &[
                    (
                        "done".to_string(),
                        fluent_bundle::FluentValue::from(done_count as i64),
                    ),
                    (
                        "total".to_string(),
                        fluent_bundle::FluentValue::from(total as i64),
                    ),
                ],
            )
            .to_string(),
        )
        .style(title_style)
        .centered()
    };

    // ── 文件条目列表 ──
    if payload.items.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("popup-download-empty"),
            muted_style,
        )]));
    } else {
        let visible = payload.items.iter().take(MAX_VISIBLE_ITEMS);
        for item in visible {
            let (icon, style) = match &item.status {
                FileDownloadStatus::Pending => (" · ", dim_style),
                FileDownloadStatus::Downloading => (" → ", active_style),
                FileDownloadStatus::Done => (" ✓ ", success_style),
                FileDownloadStatus::Failed(_) => (" ✗ ", error_style),
            };
            let display = format!("{}{}", icon, item.filename);
            lines.push(Line::from(vec![Span::styled(display, style)]));
        }
    }

    lines.push(Line::from(""));

    // ── Footer 提示 ──
    if finished {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("popup-download-footer-done"),
            muted_style,
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("popup-download-footer-active"),
            muted_style,
        )]));
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 弹窗外壳：上下边框 + 标题（参照 ConfirmPopup 模式）
    let popup_border_fg = theme_def.read().component.popup.border;
    let popup_title_fg = theme_def.read().component.popup.action_primary;
    let popup_block = ratatui_kit::ratatui::widgets::Block::default()
        .borders(
            ratatui_kit::ratatui::widgets::Borders::TOP
                | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
        )
        .border_style(ratatui_kit::ratatui::style::Style::new().fg(popup_border_fg))
        .title_top(title_line.style(Style::new().fg(popup_title_fg)));
    let text_render = content.block(popup_block);

    element!(
        View(
            flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
            width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
        ) {
            Text(text: text_render)
        }
    )
}
