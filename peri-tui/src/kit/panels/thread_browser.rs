//! Thread Browser 面板（spec/global/domains/tui/tui-panels.md §6.6）
//!
//! S6c：thread 列表从 `THREAD_LIST` atom 读取（由 `service_snapshot` 后台任务
//! 周期性经 ACP 查询项目 / 工作区会话）。Enter 切换 thread 操作 S11
//! 解耦后通过 AcpClient 触发。
//!
//! 仿 Login 面板模式：Vec<Line> → Paragraph → ScrollView(Text)。手动键盘
//! 导航 + 选中高亮，不使用 VirtualList。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, LANG_VERSION, THREAD_BROWSER_SCOPE, THREAD_LIST, THREAD_LIST_ERROR,
    THREAD_LIST_HAS_MORE, THREAD_LIST_PAGE_COUNT, THREAD_LIST_PAGE_SIZE, THREAD_LOAD_TX,
    ThreadBrowserScope, ThreadSummary,
};
use crate::kit::list_nav::{next_selection, previous_selection};
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

#[path = "thread_browser/history_preview.rs"]
mod history_preview;

#[component]
pub fn ThreadBrowserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    let selected_id = hooks.use_state(|| None::<String>);
    let last_viewport = hooks.use_state(|| 0usize);
    // 确认删除模式（仿 Cron 面板）：d/Delete 进入，Enter 确认 / Esc 取消
    let confirm_delete = hooks.use_state(|| None::<String>);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let preview_sv = hooks.use_state(ScrollViewState::default);
    let preview_id = hooks.use_state(|| None::<String>);
    let language_version = hooks.use_atom(&LANG_VERSION).get();
    let preview_request = preview_id.read().clone();
    let preview = hooks.use_async_state(
        move || async move {
            let Some(id) = preview_request else {
                return Ok::<_, String>(None);
            };
            let client = ACP_CLIENT_HANDLE
                .get()
                .ok_or_else(|| i18n::tr("thread-history-disconnected"))?;
            let payloads = client
                .read_session_history(&id)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some((id, history_preview::text(&payloads))))
        },
        (preview_id.read().clone(), language_version),
    );

    // S6c: 订阅 THREAD_LIST atom——后台 service_snapshot 2s 派生一次
    let scope_store = hooks.use_atom(&THREAD_BROWSER_SCOPE);
    let list_error = hooks.use_atom(&THREAD_LIST_ERROR).read().clone();
    let has_more = hooks.use_atom(&THREAD_LIST_HAS_MORE).get();
    let scope = scope_store.get();
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

    let panel_area = hooks.use_previous_size();

    // Shell borders + header + two detail rows + action row are fixed; the
    // remaining rows belong to this single list viewport.
    let viewport_rows = panel_area.height.saturating_sub(6).max(1) as usize;
    let preview_viewport_rows = panel_area.height.saturating_sub(5).max(1) as usize;
    let selected_index = selected_id
        .read()
        .as_ref()
        .and_then(|id| threads.iter().position(|thread| &thread.id == id))
        .unwrap_or_else(|| (*cursor.read()).min(item_count.saturating_sub(1)));
    if *cursor.read() != selected_index {
        *cursor.write_no_update() = selected_index;
    }
    let selected_id_now = threads.get(selected_index).map(|thread| thread.id.clone());
    if *selected_id.read() != selected_id_now {
        *selected_id.write_no_update() = selected_id_now;
    }
    if *last_viewport.read() != viewport_rows {
        *last_viewport.write_no_update() = viewport_rows;
        keep_selection_visible(
            &mut sv.write_no_update(),
            selected_index,
            item_count,
            viewport_rows,
        );
    }
    let scroll_start = sv
        .read()
        .offset()
        .y
        .min(item_count.saturating_sub(viewport_rows) as u16) as usize;
    let is_confirming = confirm_delete.read().is_some();
    let event_threads = threads.clone();

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
                    if preview_id.read().is_some() {
                        return if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                            EventResult::Consumed
                        } else {
                            EventResult::Ignored
                        };
                    }
                    if !is_confirming
                        && let Some(area) = area
                        && !is_scrollbar_column(&mouse, area)
                        && let Some(idx) = hit_item(
                            &mouse,
                            area,
                            ListLayout {
                                header_rows: 1,
                                item_rows: 1,
                                footer_rows: 1,
                                visible_items: viewport_rows as u16,
                                scroll_start,
                                item_count,
                            },
                        )
                    {
                        if let Some(entry) = event_threads.get(idx) {
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
                if preview_id.read().is_some() {
                    match key.code {
                        KeyCode::Char('v') => {
                            *preview_id.write() = None;
                        }
                        KeyCode::Up => preview_sv.write().scroll_up(),
                        KeyCode::Down => preview_sv.write().scroll_down(),
                        KeyCode::PageUp => {
                            for _ in 0..preview_viewport_rows {
                                preview_sv.write().scroll_up();
                            }
                        }
                        KeyCode::PageDown => {
                            for _ in 0..preview_viewport_rows {
                                preview_sv.write().scroll_down();
                            }
                        }
                        _ => {}
                    }
                    return EventResult::Consumed;
                }

                // Confirm-delete mode（标准 session/delete，agentclientprotocol.com）：
                // Enter 确认删除，Esc 取消，其他按键一律退出确认模式
                if confirm_delete.read().is_some() {
                    match key.code {
                        KeyCode::Enter => {
                            if let Some(sid) = confirm_delete.read().clone() {
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
                            *confirm_delete.write() = None;
                        }
                        KeyCode::Esc => {
                            *confirm_delete.write() = None;
                        }
                        _ => {
                            *confirm_delete.write() = None;
                        }
                    }
                    return EventResult::Consumed;
                }

                match key.code {
                    KeyCode::Tab => {
                        THREAD_BROWSER_SCOPE.set(match scope {
                            ThreadBrowserScope::Project => ThreadBrowserScope::Workspace,
                            ThreadBrowserScope::Workspace => ThreadBrowserScope::All,
                            ThreadBrowserScope::All => ThreadBrowserScope::Project,
                        });
                        THREAD_LIST_PAGE_COUNT.set(1);
                        THREAD_LIST_HAS_MORE.set(false);
                        THREAD_LIST.state().write().clear();
                        *cursor.write() = 0;
                        *selected_id.write() = None;
                        *sv.write() = ScrollViewState::default();
                    }
                    KeyCode::Char('n') if has_more => {
                        request_more_threads();
                    }
                    KeyCode::Up => {
                        let selected = previous_selection(*cursor.read());
                        set_selection(&cursor, &selected_id, selected, &event_threads);
                        keep_selection_visible(
                            &mut sv.write(),
                            selected,
                            item_count,
                            viewport_rows,
                        );
                    }
                    KeyCode::PageUp => {
                        let selected = cursor.read().saturating_sub(viewport_rows);
                        set_selection(&cursor, &selected_id, selected, &event_threads);
                        keep_selection_visible(
                            &mut sv.write(),
                            selected,
                            item_count,
                            viewport_rows,
                        );
                    }
                    KeyCode::Home => {
                        set_selection(&cursor, &selected_id, 0, &event_threads);
                        keep_selection_visible(&mut sv.write(), 0, item_count, viewport_rows);
                    }
                    KeyCode::Down | KeyCode::PageDown | KeyCode::End => {
                        let selected = match key.code {
                            KeyCode::Down => next_selection(*cursor.read(), item_count),
                            KeyCode::PageDown => cursor
                                .read()
                                .saturating_add(viewport_rows)
                                .min(item_count.saturating_sub(1)),
                            _ => item_count.saturating_sub(1),
                        };
                        set_selection(&cursor, &selected_id, selected, &event_threads);
                        keep_selection_visible(
                            &mut sv.write(),
                            selected,
                            item_count,
                            viewport_rows,
                        );
                        if selected.saturating_add(viewport_rows) >= item_count {
                            request_more_threads();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(id) = selected_id.read().clone() {
                            if let Some(tx) = THREAD_LOAD_TX.get() {
                                let _ = tx.send(id);
                            }
                            crate::kit::panel_registry::close_active_panel();
                        }
                    }
                    KeyCode::Char('v') => {
                        if let Some(id) = selected_id.read().clone() {
                            *preview_id.write() = Some(id);
                            *preview_sv.write() = ScrollViewState::default();
                        }
                    }
                    // d / Delete：进入确认删除模式（列表中无条目时不进入）
                    KeyCode::Char('d') if item_count > 0 => {
                        *confirm_delete.write() = selected_id.read().clone();
                    }
                    KeyCode::Delete if item_count > 0 => {
                        *confirm_delete.write() = selected_id.read().clone();
                    }
                    _ => {}
                }
                EventResult::Consumed
            }
        },
    );

    // ── 构建行列表（仿 Login 面板）──
    let sel = selected_index;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let header_style = Style::new().fg(semantic.text.primary).bold();
    let muted_style = Style::new().fg(semantic.text.muted).italic();
    let item_meta_style = Style::new().fg(semantic.text.muted);
    let selected_style = Style::new()
        .fg(theme_def.read().component.panel.title)
        .bold();

    let mut lines: Vec<Line<'_>> = Vec::new();

    // header
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            if has_more {
                "thread-browser-scope-loaded"
            } else {
                "thread-browser-scope-count"
            },
            &[
                (
                    "scope".into(),
                    i18n::tr(match scope {
                        ThreadBrowserScope::Project => "thread-browser-project",
                        ThreadBrowserScope::Workspace => "thread-browser-workspace",
                        ThreadBrowserScope::All => "thread-browser-all",
                    })
                    .into(),
                ),
                ("count".into(), (item_count as i64).into()),
            ],
        ),
        header_style,
    )]));
    if let Some(error) = &list_error {
        lines.push(Line::styled(
            error.clone(),
            Style::new().fg(semantic.status.warning),
        ));
    } else if threads.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("thread-browser-empty"),
            item_meta_style,
        )]));
    } else {
        for (i, entry) in threads.iter().enumerate() {
            let is_selected = i == sel;
            let cursor_mark = if is_selected { ">" } else { " " };
            let row_style = if is_selected {
                selected_style
            } else {
                Style::new().fg(semantic.text.primary)
            };

            let title = entry
                .title
                .clone()
                .unwrap_or_else(|| i18n::tr("thread-browser-untitled"));
            let updated = entry
                .updated_at
                .map(|dt| dt.format("%m-%d").to_string())
                .unwrap_or_else(|| "-".to_string());
            let width = panel_area.width.saturating_sub(2) as usize;
            let prefix = format!(" {} ", cursor_mark);
            let metadata = if width < 70 {
                String::new()
            } else {
                format!(
                    "{}  {}",
                    updated,
                    i18n::tr_args(
                        "thread-browser-messages",
                        &[("count".into(), (entry.message_count as i64).into())],
                    )
                )
            };
            let metadata_width = metadata.width();
            let available = width.saturating_sub(prefix.width() + metadata_width + 1);
            let title = truncate_text(&title, available);
            use unicode_width::UnicodeWidthStr;
            let title_width = title.width();
            let prefix_width = prefix.width();
            lines.push(Line::from(vec![
                Span::styled(
                    prefix.clone(),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(title, row_style),
                Span::raw(
                    " ".repeat(width.saturating_sub(prefix_width + title_width + metadata_width)),
                ),
                Span::styled(metadata, item_meta_style),
            ]));

            // Message count and date share the same compact row; path and ID
            // are shown only in the fixed selection details below.
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
            i18n::tr(if panel_area.width < 80 {
                "thread-browser-actions-compact"
            } else {
                "thread-browser-actions"
            }),
            muted_style,
        )]));
    }

    if let Some(id) = preview_id.read().as_ref() {
        lines = vec![Line::from("")];
        if *preview.loading.read() {
            lines.push(Line::from(i18n::tr("thread-history-loading")));
        } else if let Some(error) = preview.error.read().as_ref() {
            lines.push(Line::from(error.clone()));
        } else if let Some(Some((loaded_id, text))) = preview.data.read().as_ref()
            && loaded_id == id
        {
            lines.extend(text.lines().map(|line| Line::from(line.to_owned())));
        }
    }
    let content = Paragraph::new(ratatui::text::Text::from(lines.clone()));
    let content = if preview_id.read().is_some() {
        content.wrap(ratatui::widgets::Wrap { trim: false })
    } else {
        content
    };
    let content_height = content
        .line_count(panel_area.width.max(1))
        .clamp(1, u16::MAX as usize) as u16;

    // Preview keeps the full transcript as one scrollable document.
    let body_area = ratatui_kit::ratatui::layout::Rect::new(
        panel_area.x,
        panel_area.y.saturating_add(2),
        panel_area.width,
        viewport_rows as u16,
    );
    let preview_area = ratatui_kit::ratatui::layout::Rect::new(
        panel_area.x,
        panel_area.y.saturating_add(3),
        panel_area.width,
        preview_viewport_rows as u16,
    );
    if preview_id.read().is_none() {
        crate::kit::panel_scroll::register_panel_scroll(PanelKind::ThreadBrowser, body_area, sv);
        let footer = lines.pop().unwrap_or_else(|| Line::from(""));
        let header = lines.first().cloned().unwrap_or_else(|| Line::from(""));
        let list_lines = lines.into_iter().skip(1).collect::<Vec<_>>();
        let selected = threads.get(sel);
        let details = selected
            .map(|entry| {
                let title = entry
                    .title
                    .clone()
                    .unwrap_or_else(|| i18n::tr("thread-browser-untitled"));
                let id: String = entry.id.chars().take(8).collect();
                use unicode_width::UnicodeWidthStr;
                let detail_width = panel_area.width as usize;
                let title_empty =
                    i18n::tr_args("thread-browser-selected", &[("title".into(), "".into())]);
                let title = truncate_text(&title, detail_width.saturating_sub(title_empty.width()));
                let path_empty = i18n::tr_args(
                    "thread-browser-selected-path",
                    &[("path".into(), "".into()), ("id".into(), "".into())],
                );
                let id_display = format!("{id}…");
                let path = path_tail(
                    &entry.cwd,
                    detail_width.saturating_sub(path_empty.width() + id_display.width()),
                );
                vec![
                    Line::from(i18n::tr_args(
                        "thread-browser-selected",
                        &[("title".into(), title.into())],
                    )),
                    Line::from(i18n::tr_args(
                        "thread-browser-selected-path",
                        &[
                            ("path".into(), path.into()),
                            ("id".into(), id_display.into()),
                        ],
                    )),
                ]
            })
            .unwrap_or_else(|| vec![Line::from(""), Line::from("")]);
        return panel_shell!(PanelKind::ThreadBrowser, {
            View(height: Constraint::Length(1), width: Constraint::Fill(1)) { Text(text: header) }
            ScrollView(scrollbars: crate::kit::panel_registry::clean_scrollbars(), state: Some(sv), width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                for (index, line) in list_lines.iter().enumerate() {
                    View(key: index, height: Constraint::Length(1), width: Constraint::Fill(1)) { Text(text: line.clone()) }
                }
            }
            View(height: Constraint::Length(2), width: Constraint::Fill(1)) {
                Text(text: details[0].clone())
                Text(text: details[1].clone())
            }
            View(height: Constraint::Length(1), width: Constraint::Fill(1)) { Text(text: footer) }
        });
    }

    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::ThreadBrowser,
        preview_area,
        preview_sv,
    );
    panel_shell!(PanelKind::ThreadBrowser, {
        View(height: Constraint::Length(2), width: Constraint::Fill(1)) {
            Text(text: Line::styled(i18n::tr("thread-history-preview-hint"), header_style))
            Text(text: Line::from(preview_id.read().clone().unwrap_or_default()))
        }
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(preview_sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(height: Constraint::Length(content_height), width: Constraint::Fill(1)) {
                Text(text: content)
            }
        }
        View(height: Constraint::Length(1), width: Constraint::Fill(1)) {
            Text(text: Line::styled(i18n::tr("thread-browser-preview-actions"), muted_style))
        }
    })
}

fn request_more_threads() {
    let loaded = THREAD_LIST.state().read().len();
    if !THREAD_LIST_HAS_MORE.get() || loaded == 0 {
        return;
    }
    // Coalesce repeated keys while the same next page is still being fetched.
    let loaded_pages = u32::try_from(loaded)
        .unwrap_or(u32::MAX)
        .div_ceil(THREAD_LIST_PAGE_SIZE);
    let requested = loaded_pages.saturating_add(1);
    if requested > THREAD_LIST_PAGE_COUNT.get() {
        THREAD_LIST_PAGE_COUNT.set(requested);
    }
}

fn set_selection(
    cursor: &State<usize>,
    selected_id: &State<Option<String>>,
    index: usize,
    threads: &[ThreadSummary],
) {
    let index = index.min(threads.len().saturating_sub(1));
    *cursor.write() = index;
    *selected_id.write() = threads.get(index).map(|thread| thread.id.clone());
}

fn keep_selection_visible(
    state: &mut ScrollViewState,
    selected: usize,
    count: usize,
    viewport: usize,
) {
    if count == 0 || viewport == 0 {
        return;
    }
    let max_start = count.saturating_sub(viewport);
    let mut start = state.offset().y as usize;
    if selected < start {
        start = selected;
    } else if selected >= start.saturating_add(viewport) {
        start = selected.saturating_add(1).saturating_sub(viewport);
    }
    state.set_offset(ratatui_kit::ratatui::layout::Position::new(
        0,
        start.min(max_start) as u16,
    ));
}

fn path_tail(path: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if path.width() <= width {
        return path.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 1;
    let suffix: Vec<char> = path
        .chars()
        .rev()
        .take_while(|ch| {
            used += ch.width().unwrap_or(0);
            used <= width
        })
        .collect();
    format!("…{}", suffix.into_iter().rev().collect::<String>())
}

fn truncate_text(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if text.is_empty() {
        return String::new();
    }
    if text
        .chars()
        .map(|ch| ch.width().unwrap_or(0))
        .sum::<usize>()
        <= width
    {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
#[path = "thread_browser/ui_test.rs"]
mod ui_test;
