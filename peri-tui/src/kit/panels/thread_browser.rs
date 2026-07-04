//! Thread Browser 面板（TUI-PAGE §6.6）
//!
//! S6c：thread 列表从 `THREAD_LIST` atom 读取（由 `service_snapshot` 后台任务
//! 周期性从 ServiceRegistry.thread_store 派生）。Enter 切换 thread 操作 S11
//! 解耦后通过 AcpClient 触发（暂留 TODO）。

use ratatui_kit::{
    prelude::tui_widget_list::{ListBuildContext, ListState},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{THREAD_LIST, THREAD_LOAD_TX, ThreadSummary};
use crate::kit::theme;

#[component]
pub fn ThreadBrowserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let list_state = hooks.use_state(ListState::default);

    // S6c: 订阅 THREAD_LIST atom——后台 service_snapshot 2s 派生一次
    let threads_store = hooks.use_atom(&THREAD_LIST);
    let threads: Vec<ThreadSummary> = threads_store.read().clone();
    let _ = threads_store; // StoreState 是 Copy，无需显式 drop

    let header_style = Style::new().fg(theme::semantic().text.primary).bold();
    let muted_style = Style::new().fg(theme::semantic().text.muted).italic();
    let item_meta_style = Style::new().fg(theme::semantic().text.muted);

    panel_shell!(PanelKind::ThreadBrowser, {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(
                text: Line::from(vec![Span::styled(
                    format!("  {} threads", threads.len()),
                    header_style,
                )]),
            )
            Text(
                text: Line::from(vec![Span::styled(
                    "  Enter::open · Esc::close",
                    muted_style,
                )]),
            )
            if threads.is_empty() {
                Text(
                    text: Line::from(vec![Span::styled(
                        "  No recent threads",
                        item_meta_style,
                    )]),
                )
            } else {
                VirtualList<Paragraph<'static>>(
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                    state: list_state,
                    item_count: threads.len(),
                    active: true,
                    default_index: Some(0),
                    scroll_padding: 2u16,
                    infinite_scrolling: false,
                    render_item: move |ctx: &ListBuildContext| {
                        let entry = &threads[ctx.index];
                        let selected_style = if ctx.is_selected {
                            Style::new().fg(theme::component().panel.title).bold()
                        } else {
                            Style::new().fg(theme::semantic().text.primary)
                        };
                        let id_short: String = entry.id.chars().take(8).collect();
                        let title = entry
                            .title
                            .clone()
                            .unwrap_or_else(|| format!("(untitled {})", id_short));
                        let updated = entry
                            .updated_at
                            .map(|dt| dt.format("%Y-%m-%d").to_string())
                            .unwrap_or_else(|| "-".to_string());
                        let cwd: String = entry.cwd.chars().take(40).collect();
                        (
                            Paragraph::new(vec![
                                Line::from(vec![Span::styled(
                                    format!(
                                        "{} {}  {}",
                                        if ctx.is_selected { ">" } else { " " },
                                        updated,
                                        title
                                    ),
                                    selected_style,
                                )]),
                                Line::from(vec![Span::styled(
                                    format!(
                                        "    id: {}...  {} messages  {}",
                                        id_short, entry.message_count, cwd
                                    ),
                                    item_meta_style,
                                )]),
                            ]),
                            2u16,
                        )
                    },
                    on_select: move |index: usize| {
                        let threads = THREAD_LIST.state().read().clone();
                        if let Some(entry) = threads.get(index) {
                            if let Some(tx) = THREAD_LOAD_TX.get() {
                                let _ = tx.send(entry.id.clone());
                            }
                            crate::kit::panel_registry::close_active_panel();
                        }
                    },
                )
            }
            Text(
                text: Line::from(vec![Span::styled(
                    "  ↑/↓::navigate Enter::open · Esc::close",
                    muted_style,
                )]),
            )
        }
    })
}
