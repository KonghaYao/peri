//! ratatui-kit DownloadProgressPopup component.
//!
//! 下载进度弹窗：展示从 GitHub 下载主题文件的进度。
//! - 下载中：逐文件显示状态（Pending → Downloading → Done/Failed）
//! - 完成后本组件处理 Esc；下载中交给 root 关闭展示，后台任务继续

use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;
use ratatui_kit::ratatui::widgets::Paragraph;

use crate::i18n;

use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::overlay::OverlayFrame;

/// 弹窗最大行数（标题 + 条目）
const MAX_VISIBLE_ITEMS: usize = 16;

pub(crate) fn handle_download_progress_event(event: Event) -> EventResult {
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    match (key.modifiers, key.code) {
        // 未完成时交给 root Esc 链关闭展示；展示清空不释放下载 owner。
        (KeyModifiers::NONE, KeyCode::Esc) => {
            let dl = DOWNLOAD_PROGRESS.state();
            let current = dl.read().clone();
            if current.finished {
                crate::kit::popup_overlay::close_popup();
                return EventResult::Consumed;
            }
            EventResult::Ignored
        }
        _ => EventResult::Ignored,
    }
}

#[component]
pub fn DownloadProgressPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let progress_store = hooks.use_atom(&DOWNLOAD_PROGRESS);
    let payload = progress_store.read().clone();
    let finished = payload.finished;
    let _ = progress_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    hooks.use_event_handler(
        EventScope::Current,
        EventPriority::High,
        handle_download_progress_event,
    );

    let guard = theme_def.read();
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
    let title_text = if finished {
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
        .to_string()
    } else {
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
        .to_string()
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

    // 弹窗外壳：上下边框 + 标题（与确认弹窗共用 OverlayFrame）
    let popup_border_fg = theme_def.read().component.popup.border;
    let popup_title_fg = theme_def.read().component.popup.action_primary;
    let frame = OverlayFrame::new(Line::from(title_text))
        .border(Style::new().fg(popup_border_fg))
        .title_style(Style::new().fg(popup_title_fg));
    let text_render = content.block(frame.widget());

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

use crate::kit::atoms::DOWNLOAD_PROGRESS;

use crate::kit::atoms::FileDownloadStatus;

use crate::kit::atoms::LANG_VERSION;
