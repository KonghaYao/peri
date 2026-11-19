//! Mermaid 图详情面板：语义概览、分组展开、全图与源码。
//!
//! 双轴滚动由 `ScrollView` 负责；键位只拦截面板级动作（源码/全图/分组/复制/Esc），
//! 方向键继续交给 ScrollView，保持与其他详情面板一致。

use crate::app::panel_types::PanelKind;
use crate::kit::mermaid::{self, DetailMode, MERMAID_DETAIL};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Constraint;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;
use ratatui_kit::ratatui::widgets::Paragraph;

#[component]
pub fn MermaidDiagramPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _theme = hooks.use_atom(&THEME_ATOM);
    let detail_atom = hooks.use_atom(&MERMAID_DETAIL);
    let sv = hooks.use_state(ScrollViewState::default);
    let selected_group = hooks.use_state(|| 0usize);
    let expanded = hooks.use_state(Vec::<String>::new);
    let mode = hooks.use_state(|| {
        if detail_atom
            .read()
            .as_ref()
            .is_some_and(|detail| detail.overview.is_some())
        {
            DetailMode::Overview
        } else {
            DetailMode::Source
        }
    });
    let full = hooks.use_state(|| Option::<std::sync::Arc<Vec<String>>>::None);
    let full_error = hooks.use_state(|| Option::<String>::None);
    let status = hooks.use_state(|| Option::<String>::None);
    let area = hooks.use_previous_size();
    let content_width = area.width.saturating_sub(2).max(1) as usize;

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        // 弹窗（如确认框）优先于面板：Esc/Enter 不能越过弹窗关闭图详情。
        if matches!(
            crate::kit::focus_router::active_layer(),
            crate::kit::focus_router::FocusLayer::Popup(_)
        ) {
            return EventResult::Ignored;
        }
        let detail = MERMAID_DETAIL.state().read().clone();
        match key.code {
            KeyCode::Esc => {
                mermaid::close_detail_and_restore();
                EventResult::Consumed
            }
            KeyCode::Char('y') if key.modifiers == KeyModifiers::NONE => {
                if let Some(detail) = detail.as_ref() {
                    mermaid::copy_to_clipboard(detail);
                    *status.write() = Some(crate::i18n::tr("mermaid-detail-copied"));
                }
                EventResult::Consumed
            }
            KeyCode::Char('s') if key.modifiers == KeyModifiers::NONE => {
                let next = if *mode.read() == DetailMode::Source {
                    DetailMode::Overview
                } else {
                    DetailMode::Source
                };
                *mode.write() = next;
                EventResult::Consumed
            }
            KeyCode::Char('f') if key.modifiers == KeyModifiers::NONE => {
                if *mode.read() == DetailMode::Full {
                    *mode.write() = DetailMode::Overview;
                    return EventResult::Consumed;
                }
                if full.read().is_none()
                    && let Some(detail) = detail.as_ref()
                {
                    match mermaid::render_full(&detail.source, detail.width.max(1)) {
                        Ok(output) => {
                            *full.write() = Some(std::sync::Arc::new(output.lines().to_vec()))
                        }
                        Err(error) => {
                            *full_error.write() = Some(error.reason().to_string());
                        }
                    }
                }
                *mode.write() = DetailMode::Full;
                EventResult::Consumed
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let count = detail
                    .as_ref()
                    .and_then(|detail| detail.overview.as_ref())
                    .map(|overview| overview.groups.len())
                    .unwrap_or(0);
                if count == 0 {
                    return EventResult::Ignored;
                }
                let current = *selected_group.read();
                let next = if matches!(key.code, KeyCode::BackTab) {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                *selected_group.write() = next;
                EventResult::Consumed
            }
            KeyCode::Enter => {
                let Some(group_id) = selected_group_id(detail.as_ref(), *selected_group.read())
                else {
                    return EventResult::Ignored;
                };
                let mut list = expanded.read().clone();
                if let Some(position) = list.iter().position(|id| id == &group_id) {
                    list.remove(position);
                } else {
                    list.push(group_id);
                }
                *expanded.write() = list;
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let Some(detail) = detail_atom.read().clone() else {
        let lines = vec![Line::from(Span::styled(
            "没有打开的 Mermaid 图".to_string(),
            Style::default(),
        ))];
        crate::kit::panel_scroll::register_panel_scroll(PanelKind::MermaidDiagram, area, sv);
        return panel_shell!(PanelKind::MermaidDiagram, {
            Text(text: Paragraph::new(ratatui::text::Text::from(lines)))
        });
    };

    let mut lines: Vec<Line<'static>> = mermaid::view_lines(
        &detail,
        *mode.read(),
        *selected_group.read(),
        &expanded.read(),
        full.read().as_ref(),
        content_width,
    )
    .into_iter()
    .map(Line::from)
    .collect();
    if let Some(error) = full_error.read().as_ref() {
        lines.push(Line::from(Span::styled(
            format!("全图不可用：{error}"),
            Style::default(),
        )));
    }
    if let Some(message) = status.read().as_ref() {
        lines.push(Line::from(Span::styled(message.clone(), Style::default())));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "y 复制源码 · s 源码 · f 全图 · Tab 分组 · Enter 展开 · Esc 返回",
    ));
    let content = Paragraph::new(ratatui::text::Text::from(lines));
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::MermaidDiagram, area, sv);
    panel_shell!(PanelKind::MermaidDiagram, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}

fn selected_group_id(detail: Option<&mermaid::MermaidDetail>, selected: usize) -> Option<String> {
    detail
        .and_then(|detail| detail.overview.as_ref())
        .and_then(|overview| overview.groups.get(selected))
        .map(|group| group.id.clone())
}
