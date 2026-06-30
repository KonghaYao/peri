//! v2 ThreadBrowserPanel -- Thread (session) browser panel (PanelState trait implementation).
//!
//! Displays a list of threads (conversations) with search, navigation,
//! session switching, and deletion.
//!
//! Navigation: Up/Down to move cursor; Enter to switch session; "/" to enter
//! search; Ctrl+D to confirm delete; Esc to close.
//!
//! Data is provided as `Vec<ThreadMeta>` (from `peri-agent::thread`), which is
//! a type dependency allowed by CLAUDE.md. Thread switching produces
//! `PanelEffect::SwitchSession`, deletion produces `PanelEffect::SendToAcp`.

use chrono::Utc;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_textarea::Input;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use peri_agent::thread::ThreadMeta; // P4b: type-dependency, full runtime fields
use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// ThreadEntry -- display-friendly wrapper
// ---------------------------------------------------------------------------

/// A thread entry displayed in the panel list.
#[derive(Debug, Clone)]
pub struct ThreadEntry {
    /// Original thread metadata.
    pub meta: ThreadMeta,
    /// Whether this thread is the currently active session.
    pub is_current: bool,
}

// ---------------------------------------------------------------------------
// ThreadBrowserPanel
// ---------------------------------------------------------------------------

/// v2 Thread browser panel.
///
/// Shows a searchable list of threads with navigation, session switching,
/// and deletion. Side-effects (switch/delete) are returned as `PanelEffect`
/// instructions; the state machine translates them to actual operations.
#[derive(Debug)]
pub struct ThreadBrowserPanel {
    /// Thread entries.
    entries: Vec<ThreadEntry>,
    /// Cursor position (0-based index into filtered entries).
    cursor: usize,
    /// Vertical scroll offset (in lines, 0-based).
    scroll_offset: u16,
    /// Whether the user is in "confirm delete" mode.
    confirm_delete: bool,
    /// Whether search mode is active.
    search_focused: bool,
    /// Search query string.
    search_query: String,
    /// Filtered indices (mapping from display position to entries index).
    filtered_indices: Vec<usize>,
    /// Optional git branch name for display.
    branch: Option<String>,
}

impl ThreadBrowserPanel {
    /// Create an empty panel (no threads loaded yet).
    ///
    /// Used by the registry factory. Threads can be populated later via
    /// `set_threads()`.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            confirm_delete: false,
            search_focused: true,
            search_query: String::new(),
            filtered_indices: Vec::new(),
            branch: None,
        }
    }

    /// Create a panel from a list of `ThreadMeta`.
    ///
    /// `current_thread_id` marks which thread is currently active.
    /// `branch` is the optional git branch name for display.
    pub fn new(
        threads: Vec<ThreadMeta>,
        current_thread_id: Option<&str>,
        branch: Option<String>,
    ) -> Self {
        let mut panel = Self::empty();
        panel.branch = branch;
        panel.set_threads(threads, current_thread_id);
        panel
    }

    /// Replace threads data and recompute filtered indices.
    pub fn set_threads(&mut self, threads: Vec<ThreadMeta>, current_thread_id: Option<&str>) {
        let current_id = current_thread_id.map(|s| s.to_string());
        self.entries = threads
            .into_iter()
            .map(|meta| {
                let is_current = current_id.as_deref() == Some(&meta.id);
                ThreadEntry { meta, is_current }
            })
            .collect();
        self.cursor = 0;
        self.scroll_offset = 0;
        self.confirm_delete = false;
        self.refresh_filter();
    }

    /// Total number of entries (unfiltered).
    pub fn total_entries(&self) -> usize {
        self.entries.len()
    }

    /// Total number of filtered entries.
    pub fn total_filtered(&self) -> usize {
        self.filtered_indices.len()
    }

    /// Current cursor position (0-based, into filtered list).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move cursor by `delta`. Wraps around (euclidean).
    fn move_cursor(&mut self, delta: isize) {
        let total = self.total_filtered();
        if total == 0 {
            return;
        }
        self.cursor = ((self.cursor as isize + delta).rem_euclid(total as isize)) as usize;
    }

    /// Recompute filtered indices based on search query.
    fn refresh_filter(&mut self) {
        let query = self.search_query.to_lowercase();
        self.filtered_indices = if query.is_empty() {
            (0..self.entries.len()).collect()
        } else {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.meta
                        .title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                })
                .map(|(i, _)| i)
                .collect()
        };
        if self.cursor >= self.filtered_indices.len() {
            self.cursor = self.filtered_indices.len().saturating_sub(1);
        }
    }

    /// Ensure cursor is visible within `visible_lines` of the top.
    fn ensure_visible(&mut self, visible_lines: u16) {
        if self.filtered_indices.is_empty() {
            return;
        }
        // 每条 thread: 标题行 + meta 行 + 空行 = 3 行
        let lines_per_entry: u16 = 3;
        let header_lines: u16 = 1; // search hint line
        let cursor_line = header_lines + (self.cursor as u16) * lines_per_entry;

        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        } else if cursor_line >= self.scroll_offset + visible_lines {
            self.scroll_offset = cursor_line - visible_lines + 1;
        }
    }
}

/// 截断字符串到最大显示宽度（CJK 安全，基于 unicode-width）。
fn truncate_display(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_string();
    }
    // Reserve 3 columns for the trailing "..." so the final result width ≤ max_width.
    let target = max_width.saturating_sub(3);
    let mut cum = 0;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if cum + cw > target {
            return format!("{}...", &s[..i]);
        }
        cum += cw;
    }
    s.to_string()
}

/// 格式化内容大小为人类可读字符串。
fn format_content_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else if bytes > 0 {
        format!("{}B", bytes)
    } else {
        String::new()
    }
}

/// 格式化相对时间。
fn format_relative_time(lc: &crate::i18n::LcRegistry, dt: &chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now.signed_duration_since(*dt);
    let secs = diff.num_seconds();
    if secs < 60 {
        lc.tr("thread-browser-time-just-now").to_string()
    } else if secs < 3600 {
        let minutes = secs / 60;
        lc.tr_args(
            "thread-browser-time-minutes",
            &[
                ("count".into(), minutes.to_string().into()),
                ("suffix".into(), "".into()),
            ],
        )
    } else if secs < 86400 {
        let hours = secs / 3600;
        lc.tr_args(
            "thread-browser-time-hours",
            &[
                ("count".into(), hours.to_string().into()),
                ("suffix".into(), "".into()),
            ],
        )
    } else {
        let days = secs / 86400;
        lc.tr_args(
            "thread-browser-time-days",
            &[
                ("count".into(), days.to_string().into()),
                ("suffix".into(), "".into()),
            ],
        )
    }
}

impl PanelState for ThreadBrowserPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::ThreadBrowser
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;
        let total_filtered = self.total_filtered();
        let total_all = self.total_entries();
        let cursor_display = if total_filtered == 0 {
            0
        } else {
            self.cursor + 1
        };

        let title_text = lc.tr_args(
            "thread-browser-title",
            &[
                ("cursor".into(), cursor_display.to_string().into()),
                ("total".into(), total_all.to_string().into()),
            ],
        );

        let inner = BorderedPanel::new(Span::styled(
            title_text,
            Style::default()
                .fg(theme::SELECTED_FG)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::MUTED))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        let max_title_width = inner.width.saturating_sub(6) as usize;

        // 搜索提示行
        let search_hint = if self.search_focused {
            let query_val = &self.search_query;
            if query_val.is_empty() {
                Line::from(vec![
                    Span::styled(" ⌕ ", Style::default().fg(theme::MUTED)),
                    Span::styled(
                        lc.tr("thread-browser-search-placeholder"),
                        Style::default().fg(theme::DIM),
                    ),
                ])
            } else {
                let mut spans = vec![
                    Span::styled(" ⌕ ", Style::default().fg(theme::MUTED)),
                    Span::styled(query_val.as_str(), Style::default().fg(theme::TEXT)),
                ];
                spans.push(Span::styled("█", Style::default().fg(theme::TEXT)));
                Line::from(spans)
            }
        } else {
            Line::from(vec![
                Span::styled(" ⌕ ", Style::default().fg(theme::DIM)),
                Span::styled(
                    lc.tr("thread-browser-search-placeholder"),
                    Style::default().fg(theme::DIM),
                ),
            ])
        };
        lines.push(search_hint);
        lines.push(Line::from(""));

        if self.filtered_indices.is_empty() {
            if self.search_query.is_empty() {
                lines.push(Line::from(Span::styled(
                    lc.tr("thread-browser-empty"),
                    Style::default().fg(theme::MUTED),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    lc.tr("thread-browser-no-match"),
                    Style::default().fg(theme::MUTED),
                )));
            }
        } else {
            for (display_idx, &entry_idx) in self.filtered_indices.iter().enumerate() {
                let entry = &self.entries[entry_idx];
                let is_cursor = display_idx == self.cursor;
                let meta = &entry.meta;
                let untitled = lc.tr("thread-browser-untitled");
                let title = meta.title.as_deref().unwrap_or(&untitled);
                let label = truncate_display(title, max_title_width);

                // 第一行：cursor indicator + current marker + title
                let cursor_span = Span::styled(
                    if is_cursor { "❯ " } else { "  " },
                    Style::default().fg(if is_cursor {
                        theme::SELECTED_FG
                    } else {
                        theme::MUTED
                    }),
                );

                let title_style = if is_cursor {
                    Style::default()
                        .fg(theme::SELECTED_FG)
                        .add_modifier(Modifier::BOLD)
                } else if entry.is_current {
                    Style::default()
                        .fg(theme::SELECTED_FG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let mut first_line_spans = vec![cursor_span];
                if entry.is_current {
                    first_line_spans.push(Span::styled(
                        "✓ ".to_string(),
                        Style::default().fg(theme::SAGE),
                    ));
                }
                first_line_spans.push(Span::styled(label, title_style));
                lines.push(Line::from(first_line_spans));

                // 第二行：metadata（relative time · branch · size）
                let relative_time = format_relative_time(lc, &meta.updated_at);
                let size_str = format_content_size(meta.content_size);

                let mut meta_parts = vec![Span::styled(
                    format!("   {}", relative_time),
                    Style::default().fg(theme::MUTED),
                )];

                if let Some(branch) = &self.branch {
                    meta_parts.push(Span::styled(
                        format!(" · {}", branch),
                        Style::default().fg(theme::MUTED),
                    ));
                }

                if !size_str.is_empty() {
                    meta_parts.push(Span::styled(
                        format!(" · {}", size_str),
                        Style::default().fg(theme::MUTED),
                    ));
                }

                lines.push(Line::from(meta_parts));

                // 空行分隔
                lines.push(Line::from(""));
            }
        }

        // 截断到可视区域
        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;

        // confirm_delete mode
        if self.confirm_delete {
            match input {
                // Enter: 执行删除
                Input {
                    key: Key::Enter, ..
                } => {
                    self.confirm_delete = false;
                    if let Some(&entry_idx) = self.filtered_indices.get(self.cursor) {
                        let entry = &self.entries[entry_idx];
                        let id = entry.meta.id.clone();
                        let title = entry
                            .meta
                            .title
                            .clone()
                            .unwrap_or_else(|| "(untitled)".to_string());
                        let mut effects = vec![
                            PanelEffect::SendToAcp {
                                event: "delete_session".to_string(),
                                data: serde_json::json!({ "id": id }),
                            },
                            PanelEffect::ShowNotification(format!("Deleted thread: {}", title)),
                        ];
                        // 删除后若列表将空，关闭面板
                        if self.total_filtered() <= 1 {
                            effects.push(PanelEffect::Close);
                        }
                        return effects;
                    }
                    vec![]
                }
                // Esc 或其他：取消确认
                _ => {
                    self.confirm_delete = false;
                    vec![]
                }
            }
        } else if self.search_focused {
            // search focused mode
            match input {
                Input {
                    key: Key::Char('c'),
                    ctrl: true,
                    ..
                } => vec![],
                Input { key: Key::Esc, .. } => {
                    if !self.search_query.is_empty() {
                        self.search_query.clear();
                        self.refresh_filter();
                        vec![]
                    } else {
                        vec![PanelEffect::Close]
                    }
                }
                Input {
                    key: Key::Char(c), ..
                } => {
                    self.search_query.push(c);
                    self.refresh_filter();
                    vec![]
                }
                Input {
                    key: Key::Backspace,
                    ..
                } => {
                    self.search_query.pop();
                    self.refresh_filter();
                    vec![]
                }
                Input {
                    key: Key::Delete, ..
                } => {
                    self.search_query.clear();
                    self.refresh_filter();
                    vec![]
                }
                // Down / Tab -> exit search focus
                Input { key: Key::Down, .. } | Input { key: Key::Tab, .. } => {
                    self.search_focused = false;
                    vec![]
                }
                // Enter: open selected thread
                Input {
                    key: Key::Enter, ..
                } => {
                    if let Some(&entry_idx) = self.filtered_indices.get(self.cursor) {
                        let id = self.entries[entry_idx].meta.id.clone();
                        return vec![PanelEffect::SwitchSession(id)];
                    }
                    vec![]
                }
                _ => vec![],
            }
        } else {
            // list mode
            match input {
                Input {
                    key: Key::Char('c'),
                    ctrl: true,
                    ..
                } => vec![],
                Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
                Input { key: Key::Up, .. } => {
                    self.move_cursor(-1);
                    self.ensure_visible(10);
                    vec![]
                }
                Input { key: Key::Down, .. } => {
                    self.move_cursor(1);
                    self.ensure_visible(10);
                    vec![]
                }
                Input {
                    key: Key::Enter, ..
                } => {
                    if let Some(&entry_idx) = self.filtered_indices.get(self.cursor) {
                        let id = self.entries[entry_idx].meta.id.clone();
                        return vec![PanelEffect::SwitchSession(id)];
                    }
                    vec![]
                }
                Input {
                    key: Key::Char('d'),
                    ctrl: true,
                    ..
                } => {
                    if self.total_filtered() > 0 {
                        self.confirm_delete = true;
                    }
                    vec![]
                }
                // / or Tab -> enter search
                Input {
                    key: Key::Char('/'),
                    ..
                }
                | Input { key: Key::Tab, .. } => {
                    self.search_focused = true;
                    vec![]
                }
                _ => vec![],
            }
        }
    }

    fn handle_paste(&mut self, text: &str, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        if self.search_focused {
            self.search_query.push_str(text);
            self.refresh_filter();
        }
        vec![]
    }

    fn handle_scroll(&mut self, lines: i16, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        let new_offset = (self.scroll_offset as i16 + lines).max(0) as u16;
        self.scroll_offset = new_offset;
        vec![]
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let relative_y = mouse.row.saturating_sub(area.y);
            // header: 2 lines (search hint + blank), 每条 3 行
            let header: u16 = 2;
            let clicked_line = relative_y.saturating_sub(header);
            let clicked_entry = (clicked_line / 3) as usize;
            if clicked_entry < self.total_filtered() {
                self.cursor = clicked_entry;
            }
        }
        vec![]
    }

    fn desired_height(&self, screen_h: u16, _screen_w: u16) -> u16 {
        (screen_h * 3 / 5).max(16)
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        if self.confirm_delete {
            return vec![
                (
                    "Enter".to_string(),
                    lc.tr("hint-history-confirm-delete").to_string(),
                ),
                ("Esc".to_string(), lc.tr("key-cancel").to_string()),
            ];
        }
        if self.search_focused {
            return vec![
                (
                    "\u{2193}/Tab".to_string(),
                    lc.tr("hint-plugin-exit-search").to_string(),
                ),
                ("Esc".to_string(), lc.tr("key-close").to_string()),
            ];
        }
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                lc.tr("key-move").to_string(),
            ),
            ("Enter".to_string(), lc.tr("key-confirm").to_string()),
            ("/".to_string(), lc.tr("hint-plugin-search").to_string()),
            ("Ctrl+D".to_string(), lc.tr("key-delete").to_string()),
            ("Esc".to_string(), lc.tr("key-close").to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tui_textarea::Key;

    use super::*;
    use crate::panel::read_context::{PanelReadContext, ServiceRegistrySnapshot};
    use crate::panel::PanelState;

    /// Helper: build a minimal `PanelReadContext` for testing.
    fn make_ctx() -> PanelReadContext<'static> {
        thread_local! {
            static SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
            static VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
            #[allow(clippy::missing_const_for_thread_local)]
            static CACHE: HashMap<String, serde_json::Value> = HashMap::new();
            static LC: crate::i18n::LcRegistry = crate::i18n::LcRegistry::default();
        }
        SNAPSHOT.with(|snapshot| {
            let snapshot: &'static ServiceRegistrySnapshot = unsafe { &*(snapshot as *const _) };
            VMS.with(|vms| {
                let vms: &'static Vec<peri_acp_types::view_model::ViewModel> =
                    unsafe { &*(vms as *const _) };
                CACHE.with(|cache| {
                    let cache: &'static HashMap<String, serde_json::Value> =
                        unsafe { &*(cache as *const _) };
                    LC.with(|lc| {
                        let lc: &'static crate::i18n::LcRegistry = unsafe { &*(lc as *const _) };
                        PanelReadContext {
                            services: snapshot,
                            view_models: vms,
                            scroll_offset: 0,
                            area: Rect::new(0, 0, 80, 24),
                            lc,
                            acp_query_cache: cache,
                        }
                    })
                })
            })
        })
    }

    fn esc_input() -> Input {
        Input {
            key: Key::Esc,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn up_input() -> Input {
        Input {
            key: Key::Up,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn down_input() -> Input {
        Input {
            key: Key::Down,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn enter_input() -> Input {
        Input {
            key: Key::Enter,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn tab_input() -> Input {
        Input {
            key: Key::Tab,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn char_input(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn backspace_input() -> Input {
        Input {
            key: Key::Backspace,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn ctrl_d_input() -> Input {
        Input {
            key: Key::Char('d'),
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// 构造测试用 ThreadMeta。
    fn make_thread(id: &str, title: Option<&str>, updated_secs_ago: i64) -> ThreadMeta {
        ThreadMeta {
            id: id.to_string(),
            title: title.map(|s| s.to_string()),
            cwd: "/tmp".to_string(),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            updated_at: Utc::now()
                - chrono::Duration::try_seconds(updated_secs_ago).unwrap_or_default(),
            message_count: 10,
            content_size: 2048,
            parent_thread_id: None,
            snapshot_at_message_id: None,
            hidden: false,
            cancel_policy: peri_agent::thread::CancelPolicy::Cascade,
            config: None,
            cached_context: None,
            agent_status: peri_agent::thread::AgentStatus::Active,
        }
    }

    #[test]
    fn test_kind_returns_thread_browser() {
        let panel = ThreadBrowserPanel::empty();
        assert_eq!(panel.kind(), PanelKind::ThreadBrowser);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = ThreadBrowserPanel::empty();
        let ctx = make_ctx();
        // 初始 search_focused=true，Esc 应关闭
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_esc_clears_search_first() {
        let threads = vec![
            make_thread("t1", Some("hello world"), 60),
            make_thread("t2", Some("foo bar"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 输入搜索字符
        panel.handle_key(char_input('h'), &ctx);
        assert_eq!(panel.search_query, "h");
        assert_eq!(panel.total_filtered(), 1); // 只有 "hello world" 匹配

        // Esc 应清空搜索（不关闭面板）
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(panel.search_query, "");
        assert_eq!(panel.total_filtered(), 2);
        assert_eq!(effects.len(), 0);

        // 再按 Esc 应关闭面板
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_navigation() {
        let threads = vec![
            make_thread("t1", Some("First Thread"), 60),
            make_thread("t2", Some("Second Thread"), 120),
            make_thread("t3", Some("Third Thread"), 180),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 初始 search_focused=true, 切换到 list mode
        panel.handle_key(down_input(), &ctx);
        assert!(!panel.search_focused);
        assert_eq!(panel.cursor(), 0);

        // Down -> cursor=1
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 1);

        // Down -> cursor=2
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 2);

        // Down -> wraps to 0
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 0);

        // Up -> wraps to 2
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 2);
    }

    #[test]
    fn test_enter_switches_session() {
        let threads = vec![
            make_thread("t1", Some("First"), 60),
            make_thread("t2", Some("Second"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 初始 search_focused=true, Enter 应直接切换（search mode 也支持 Enter）
        let effects = panel.handle_key(enter_input(), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SwitchSession(id) => assert_eq!(id, "t1"),
            other => panic!("expected SwitchSession, got {:?}", other),
        }
    }

    #[test]
    fn test_enter_switches_session_in_list_mode() {
        let threads = vec![
            make_thread("t1", Some("First"), 60),
            make_thread("t2", Some("Second"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 切到 list mode，移到 t2
        panel.handle_key(down_input(), &ctx); // exit search
        panel.handle_key(down_input(), &ctx); // cursor=1
        let effects = panel.handle_key(enter_input(), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SwitchSession(id) => assert_eq!(id, "t2"),
            other => panic!("expected SwitchSession, got {:?}", other),
        }
    }

    #[test]
    fn test_ctrl_d_enters_confirm_delete() {
        let threads = vec![make_thread("t1", Some("First"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 切到 list mode
        panel.handle_key(down_input(), &ctx);
        assert!(!panel.confirm_delete);

        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete);
    }

    #[test]
    fn test_confirm_delete_produces_send_to_acp() {
        let threads = vec![
            make_thread("t1", Some("First"), 60),
            make_thread("t2", Some("Second"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 切到 list mode + confirm delete
        panel.handle_key(down_input(), &ctx);
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete);

        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(!panel.confirm_delete);
        assert!(effects.len() >= 2);
        assert!(effects.iter().any(|e| matches!(
            e,
            PanelEffect::SendToAcp {
                event,
                data,
            } if event == "delete_session" && data["id"] == "t1"
        )));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PanelEffect::ShowNotification(_))));
    }

    #[test]
    fn test_confirm_delete_on_last_thread_closes_panel() {
        let threads = vec![make_thread("t1", Some("Only"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 切到 list mode + confirm delete
        panel.handle_key(down_input(), &ctx);
        panel.handle_key(ctrl_d_input(), &ctx);

        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_confirm_delete_esc_cancels() {
        let threads = vec![make_thread("t1", Some("First"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 切到 list mode + confirm delete
        panel.handle_key(down_input(), &ctx);
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete);

        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(!panel.confirm_delete);
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_search_filters_threads() {
        let threads = vec![
            make_thread("t1", Some("Hello World"), 60),
            make_thread("t2", Some("Foo Bar"), 120),
            make_thread("t3", Some("Hello Rust"), 180),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 初始全部可见
        assert_eq!(panel.total_filtered(), 3);

        // 搜索 "hello"
        panel.handle_key(char_input('h'), &ctx);
        panel.handle_key(char_input('e'), &ctx);
        panel.handle_key(char_input('l'), &ctx);
        panel.handle_key(char_input('l'), &ctx);
        panel.handle_key(char_input('o'), &ctx);
        assert_eq!(panel.total_filtered(), 2); // "Hello World" + "Hello Rust"
    }

    #[test]
    fn test_backspace_in_search() {
        let threads = vec![
            make_thread("t1", Some("Hello"), 60),
            make_thread("t2", Some("World"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        panel.handle_key(char_input('w'), &ctx);
        assert_eq!(panel.total_filtered(), 1);

        panel.handle_key(backspace_input(), &ctx);
        assert_eq!(panel.search_query, "");
        assert_eq!(panel.total_filtered(), 2);
    }

    #[test]
    fn test_tab_exits_search() {
        let threads = vec![make_thread("t1", Some("Hello"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        assert!(panel.search_focused);
        panel.handle_key(tab_input(), &ctx);
        assert!(!panel.search_focused);
    }

    #[test]
    fn test_search_slash_enters_search_from_list() {
        let threads = vec![make_thread("t1", Some("Hello"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // exit search
        panel.handle_key(down_input(), &ctx);
        assert!(!panel.search_focused);

        // "/" enters search
        panel.handle_key(char_input('/'), &ctx);
        assert!(panel.search_focused);
    }

    #[test]
    fn test_empty_panel() {
        let panel = ThreadBrowserPanel::empty();
        assert_eq!(panel.total_entries(), 0);
        assert_eq!(panel.total_filtered(), 0);
        assert_eq!(panel.cursor(), 0);
    }

    #[test]
    fn test_set_threads_with_current_id() {
        let threads = vec![
            make_thread("t1", Some("First"), 60),
            make_thread("t2", Some("Second"), 120),
        ];
        let panel = ThreadBrowserPanel::new(threads, Some("t2"), None);
        // t2 应标记为 current
        assert!(panel.entries[0].meta.id == "t1" && !panel.entries[0].is_current);
        assert!(panel.entries[1].meta.id == "t2" && panel.entries[1].is_current);
    }

    #[test]
    fn test_desired_height() {
        let panel = ThreadBrowserPanel::empty();
        // (24 * 3 / 5).max(16) = 16 (14 < 16)
        assert_eq!(panel.desired_height(24, 80), 16);
        // (50 * 3 / 5).max(16) = 30
        assert_eq!(panel.desired_height(50, 80), 30);
    }

    #[test]
    fn test_render_does_not_panic_empty() {
        let mut panel = ThreadBrowserPanel::empty();
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_with_threads() {
        let threads = vec![
            make_thread("t1", Some("First Thread"), 60),
            make_thread("t2", Some("Second Thread"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints_list_mode() {
        let threads = vec![make_thread("t1", Some("First"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        // exit search
        let _ctx = make_ctx();
        panel.search_focused = false;

        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 5);
    }

    #[test]
    fn test_status_bar_hints_search_mode() {
        let panel = ThreadBrowserPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2); // search mode default
    }

    #[test]
    fn test_status_bar_hints_confirm_delete() {
        let threads = vec![make_thread("t1", Some("First"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        panel.confirm_delete = true;
        panel.search_focused = false;
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn test_handle_scroll() {
        let threads = vec![
            make_thread("t1", Some("First"), 60),
            make_thread("t2", Some("Second"), 120),
        ];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        panel.handle_scroll(1, &ctx);
        assert_eq!(panel.scroll_offset, 1);

        panel.handle_scroll(5, &ctx);
        assert_eq!(panel.scroll_offset, 6);

        panel.handle_scroll(-3, &ctx);
        assert_eq!(panel.scroll_offset, 3);

        panel.handle_scroll(-10, &ctx);
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn test_ctrl_c_not_consumed_in_list_mode() {
        let threads = vec![make_thread("t1", Some("First"), 60)];
        let mut panel = ThreadBrowserPanel::new(threads, None, None);
        let ctx = make_ctx();

        // 切到 list mode
        panel.handle_key(down_input(), &ctx);

        let effects = panel.handle_key(
            Input {
                key: Key::Char('c'),
                ctrl: true,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_handle_paste_in_search() {
        let mut panel = ThreadBrowserPanel::empty();
        let ctx = make_ctx();

        assert!(panel.search_focused);
        panel.handle_paste("clipboard text", &ctx);
        assert_eq!(panel.search_query, "clipboard text");
    }

    #[test]
    fn test_handle_paste_ignored_in_list_mode() {
        let mut panel = ThreadBrowserPanel::empty();
        let ctx = make_ctx();

        panel.search_focused = false;
        panel.handle_paste("clipboard text", &ctx);
        assert_eq!(panel.search_query, "");
    }

    #[test]
    fn test_truncate_display() {
        // 短字符串不截断
        assert_eq!(truncate_display("hello", 10), "hello");
        // ASCII 截断
        let result = truncate_display("hello world", 8);
        assert!(result.ends_with("..."));
        assert!(result.width() <= 8);
        // CJK 宽字符
        let result = truncate_display("你好世界", 5);
        assert!(result.ends_with("..."));
        assert!(result.width() <= 5);
    }

    #[test]
    fn test_format_content_size() {
        assert_eq!(format_content_size(0), "");
        assert_eq!(format_content_size(512), "512B");
        assert_eq!(format_content_size(2048), "2.0KB");
        assert_eq!(format_content_size(2_000_000), "1.9MB");
    }
}
