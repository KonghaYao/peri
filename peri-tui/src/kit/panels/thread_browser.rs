//! Thread Browser 面板（spec/global/domains/tui/tui-panels.md §6.6）
//!
//! S6c：thread 列表从 `THREAD_LIST` atom 读取（由 `service_snapshot` 后台任务
//! 周期性从 ServiceRegistry.thread_store 派生）。Enter 切换 thread 操作 S11
//! 解耦后通过 AcpClient 触发。
//!
//! 仿 Login 面板模式：Vec<Line> → Paragraph → ScrollView(Text)。手动键盘
//! 导航 + 选中高亮，不使用 VirtualList。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, LANG_VERSION, THREAD_LIST, THREAD_LOAD_TX, ThreadSummary,
};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item, is_scrollbar_column};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn ThreadBrowserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    // 确认删除模式（仿 Cron 面板）：d/Delete 进入，Enter 确认 / Esc 取消
    let confirm_delete = hooks.use_state(|| false);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    hooks.use_atom(&LANG_VERSION);

    // S6c: 订阅 THREAD_LIST atom——后台 service_snapshot 2s 派生一次
    let threads_store = hooks.use_atom(&THREAD_LIST);
    let threads: Vec<ThreadSummary> = threads_store.read().clone();
    let _ = threads_store;
    let item_count = threads.len();

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 3 = 12 行；每项 3 行 → 可见 4 个。
    const VISIBLE_ITEMS: usize = 4;
    let scroll_start = scroll_start_for_selected(*cursor.read(), item_count, VISIBLE_ITEMS);
    let is_confirming = *confirm_delete.read();

    // ── 键盘 + 鼠标处理 ──
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        {
            move |event| {
                // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
                // 确认删除模式下不触发 load（防止误删时顺手切会话）
                if let Event::Mouse(mouse) = event {
                    if !is_confirming
                        && let Some(area) = area
                        && !is_scrollbar_column(&mouse, area)
                        && let Some(idx) = hit_item(
                            &mouse,
                            area,
                            ListLayout {
                                header_rows: 3,
                                item_rows: 3,
                                footer_rows: 1,
                                visible_items: VISIBLE_ITEMS as u16,
                                scroll_start,
                                item_count,
                            },
                        )
                    {
                        *cursor.write() = idx;
                        let threads_snap = THREAD_LIST.state().read().clone();
                        if let Some(entry) = threads_snap.get(idx) {
                            if let Some(tx) = THREAD_LOAD_TX.get() {
                                let _ = tx.send(entry.id.clone());
                            }
                            crate::kit::panel_registry::close_active_panel();
                        }
                        return EventResult::Consumed;
                    }
                    // 区域内的左键点击（未命中行）也消费，防止穿透到消息区选区
                    return match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                        _ => EventResult::Ignored,
                    };
                }
                let Event::Key(key) = event else {
                    return EventResult::Ignored;
                };
                if key.kind != KeyEventKind::Press {
                    return EventResult::Ignored;
                }

                // Confirm-delete mode（标准 session/delete，agentclientprotocol.com）：
                // Enter 确认删除，Esc 取消，其他按键一律退出确认模式
                if *confirm_delete.read() {
                    match key.code {
                        KeyCode::Enter => {
                            let sel = *cursor.read();
                            let threads_snap = THREAD_LIST.state().read().clone();
                            if let Some(entry) = threads_snap.get(sel) {
                                let sid = entry.id.clone();
                                if let Some(client) = ACP_CLIENT_HANDLE.get() {
                                    let client = client.clone();
                                    tokio::spawn(async move {
                                        match client.delete_session(&sid).await {
                                            Ok(()) => tracing::info!(
                                                session_id = %sid,
                                                "thread browser: session deleted"
                                            ),
                                            Err(e) => tracing::warn!(
                                                session_id = %sid,
                                                error = %e,
                                                "thread browser: session delete failed"
                                            ),
                                        }
                                    });
                                } else {
                                    tracing::warn!(
                                        target: "thread-browser",
                                        "ACP_CLIENT_HANDLE not set, delete skipped"
                                    );
                                }
                            }
                            *confirm_delete.write() = false;
                        }
                        KeyCode::Esc => {
                            *confirm_delete.write() = false;
                        }
                        _ => {
                            *confirm_delete.write() = false;
                        }
                    }
                    return EventResult::Consumed;
                }

                match key.code {
                    KeyCode::Up => {
                        let mut c = cursor.write();
                        *c = previous_selection(*c);
                    }
                    KeyCode::Down => {
                        let mut c = cursor.write();
                        *c = next_selection(*c, item_count);
                    }
                    KeyCode::Enter => {
                        let sel = *cursor.read();
                        let threads_snap = THREAD_LIST.state().read().clone();
                        if let Some(entry) = threads_snap.get(sel) {
                            if let Some(tx) = THREAD_LOAD_TX.get() {
                                let _ = tx.send(entry.id.clone());
                            }
                            crate::kit::panel_registry::close_active_panel();
                        }
                    }
                    // d / Delete：进入确认删除模式（列表中无条目时不进入）
                    KeyCode::Char('d') if item_count > 0 => {
                        *confirm_delete.write() = true;
                    }
                    KeyCode::Delete if item_count > 0 => {
                        *confirm_delete.write() = true;
                    }
                    _ => {}
                }
                EventResult::Consumed
            }
        },
    );

    // ── 构建行列表（仿 Login 面板）──
    let sel = *cursor.read();
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let header_style = Style::new().fg(semantic.text.primary).bold();
    let muted_style = Style::new().fg(semantic.text.muted).italic();
    let dim_style = Style::new().fg(semantic.text.dim);
    let item_meta_style = Style::new().fg(semantic.text.muted);
    let selected_style = Style::new()
        .fg(theme_def.read().component.panel.title)
        .bold();

    let mut lines: Vec<Line<'_>> = Vec::new();

    // header
    lines.push(Line::from(vec![Span::styled(
        format!("  {} threads", item_count),
        header_style,
    )]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-threads-header-hint"),
        muted_style,
    )]));
    lines.push(Line::from(""));

    if threads.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("thread-browser-empty"),
            item_meta_style,
        )]));
    } else {
        for (i, entry) in threads
            .iter()
            .enumerate()
            .skip(scroll_start)
            .take(VISIBLE_ITEMS)
        {
            let is_selected = i == sel;
            let cursor_mark = if is_selected { ">" } else { " " };
            let row_style = if is_selected {
                selected_style
            } else {
                Style::new().fg(semantic.text.primary)
            };

            let id_short: String = entry.id.chars().take(8).collect();
            let title = entry
                .title
                .clone()
                .unwrap_or_else(|| i18n::tr("thread-browser-untitled"));
            let updated = entry
                .updated_at
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "-".to_string());
            let cwd: String = entry.cwd.chars().take(40).collect();

            // 第一行：标记 + 日期 + 标题
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_mark),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(format!("{}  {}", updated, title), row_style),
            ]));

            // 第二行：id + 消息数 + 工作目录
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "    id: {}...  {} messages  {}",
                    id_short, entry.message_count, cwd
                ),
                if is_selected {
                    dim_style
                } else {
                    item_meta_style
                },
            )]));

            // 条目间空行
            lines.push(Line::from(""));
        }
    }

    // footer：确认删除模式显示确认提示，正常模式显示导航提示
    if is_confirming {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-threads-confirm-hint"),
            Style::new().fg(theme_def.read().semantic.status.warning),
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-threads-nav-hint"),
            muted_style,
        )]));
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::ThreadBrowser,
        hooks.use_previous_size(),
        sv,
    );

    panel_shell!(PanelKind::ThreadBrowser, {
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
