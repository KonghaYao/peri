//! v2 PluginPanel -- Plugin management panel (PanelState trait implementation).
//!
//! Displays installed plugins and discover/search functionality:
//!   - **Installed**: list of installed plugins with cursor + scroll.
//!   - **Detail**: single plugin detail with action menu (toggle, uninstall, back).
//!   - **Discover**: search box + filtered plugin list (placeholder for P3 Integration).
//!
//! Navigation: Up/Down to move cursor; Enter to drill-in or execute action;
//! Esc to go back (detail -> list) or close (list). 'd' to delete; 's' to
//! enter Discover search mode. Tab toggles search/list focus in Discover.
//!
//! Data is provided as `Vec<PluginEntry>` (local DTOs). No direct dependency
//! on `peri_middlewares::plugin` runtime types.
//!
//! **Data source**: pending P3 Integration phase -- will be injected from
//! `ServiceRegistrySnapshot` once the snapshot carries plugin data.
//!
//! **Deferred views** (postponed to P3 Integration):
//! - Marketplace management (add/remove marketplace entries)
//! - DiscoverDetail (merged into Detail with `is_discover` flag)

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_textarea::Input;

use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::i18n::LcRegistry;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ---------------------------------------------------------------------------
// Local DTOs (no peri_middlewares::plugin dependency)
// ---------------------------------------------------------------------------

/// Display-friendly installed plugin entry.
///
/// Fields mirror `peri_middlewares::plugin::PluginEntry` at the rendering layer.
/// TODO(P3 Integration): populate from `ServiceRegistrySnapshot`.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Plugin display name.
    pub name: String,
    /// Version string (e.g. "1.2.0").
    pub version: String,
    /// Short description.
    pub description: String,
    /// Source identifier ("marketplace:<name>" / "git:<url>" / "local").
    pub source: String,
    /// Whether the plugin is currently enabled.
    pub enabled: bool,
    /// Whether a newer version is available.
    pub has_update: bool,
}

/// Plugin detail action menu items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAction {
    ToggleEnabled,
    Uninstall,
    BackToList,
}

impl DetailAction {
    const ALL: [DetailAction; 3] = [
        DetailAction::ToggleEnabled,
        DetailAction::Uninstall,
        DetailAction::BackToList,
    ];

    fn label(self, enabled: bool) -> &'static str {
        match self {
            Self::ToggleEnabled => {
                if enabled {
                    "Disable plugin"
                } else {
                    "Enable plugin"
                }
            }
            Self::Uninstall => "Uninstall",
            Self::BackToList => "Back to plugin list",
        }
    }
}

/// Discover view action menu items (when viewing a discover entry as detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoverDetailAction {
    Install,
    BackToList,
}

impl DiscoverDetailAction {
    const ALL: [DiscoverDetailAction; 2] = [
        DiscoverDetailAction::Install,
        DiscoverDetailAction::BackToList,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::BackToList => "Back to plugin list",
        }
    }
}

/// View state for the plugin panel.
#[derive(Debug)]
enum PluginView {
    /// Installed plugins list.
    Installed,
    /// Detail view for an installed/discovered plugin.
    Detail {
        /// Index into the entries list (installed) or discover_entries (discover).
        index: usize,
        /// Whether this entry came from discover results.
        is_discover: bool,
    },
    /// Discover/search view.
    Discover {
        search_query: String,
        search_focused: bool,
        /// Filtered results (subset of discover_entries).
        results: Vec<PluginEntry>,
        cursor: usize,
    },
}

impl PluginView {
    fn is_installed(&self) -> bool {
        matches!(self, PluginView::Installed)
    }

    fn is_detail(&self) -> bool {
        matches!(self, PluginView::Detail { .. })
    }

    fn is_discover(&self) -> bool {
        matches!(self, PluginView::Discover { .. })
    }
}

// ---------------------------------------------------------------------------
// Lightweight text field (Send-safe)
// ---------------------------------------------------------------------------

/// Minimal single-line text editor state (String + byte cursor).
///
/// Satisfies `Send` (unlike `tui_textarea::TextArea`).
#[derive(Debug, Clone)]
struct TextField {
    text: String,
    cursor: usize,
}

impl TextField {
    fn new(value: &str) -> Self {
        Self {
            text: value.to_string(),
            cursor: value.len(),
        }
    }

    fn value(&self) -> &str {
        &self.text
    }

    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn delete_backward(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, c)| (i, c.len_utf8()));
            if let Some((byte_idx, char_len)) = prev {
                self.text.remove(byte_idx);
                self.cursor -= char_len;
            }
        }
    }

    fn delete_forward(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(0)
                .map(|(_, c)| c.len_utf8());
            if let Some(char_len) = next {
                self.text.drain(self.cursor..self.cursor + char_len);
            }
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Clear all text and reset cursor.
    #[allow(dead_code)]
    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

// ---------------------------------------------------------------------------
// PluginPanel
// ---------------------------------------------------------------------------

/// v2 Plugin panel managing installed plugin list, detail view, and discover.
///
/// All data is local DTO (`PluginEntry`). No dependency on
/// `peri_middlewares::plugin` or `crate::config::PeriConfig`.
pub struct PluginPanel {
    /// Installed plugin entries.
    entries: Vec<PluginEntry>,
    /// Cursor position in the installed list.
    cursor: usize,
    /// Scroll offset for the installed list.
    scroll_offset: u16,
    /// Current view state.
    view: PluginView,
    /// Confirm-delete state: `Some(name)` when awaiting y/n.
    confirm_delete: Option<String>,
    /// Detail view action menu cursor.
    detail_cursor: usize,
    /// Search text field for discover view.
    search_field: TextField,
    /// Placeholder discover entries (populated in P3 Integration).
    discover_entries: Vec<PluginEntry>,
}

impl PluginPanel {
    /// Create an empty panel with no data.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            view: PluginView::Installed,
            confirm_delete: None,
            detail_cursor: 0,
            search_field: TextField::new(""),
            discover_entries: Vec::new(),
        }
    }

    /// Construct a panel from the live `App` state.
    ///
    /// Returns an empty panel since plugin runtime data (enabled status,
    /// update availability) is not directly extractable from
    /// `PluginLoadResult` at construction time. Entries are populated
    /// later via `set_entries()` when ACP query results arrive.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }

    /// Create a panel pre-populated with installed entries.
    pub fn new(entries: Vec<PluginEntry>) -> Self {
        Self {
            entries,
            cursor: 0,
            scroll_offset: 0,
            view: PluginView::Installed,
            confirm_delete: None,
            detail_cursor: 0,
            search_field: TextField::new(""),
            discover_entries: Vec::new(),
        }
    }

    /// Set installed entries (replaces existing data, resets cursor).
    pub fn set_entries(&mut self, entries: Vec<PluginEntry>) {
        self.entries = entries;
        self.cursor = 0;
        self.scroll_offset = 0;
    }

    /// Set discover entries (replaces existing data).
    pub fn set_discover_entries(&mut self, entries: Vec<PluginEntry>) {
        self.discover_entries = entries;
        // Re-filter if in discover view
        let query = match &self.view {
            PluginView::Discover { search_query, .. } => search_query.clone(),
            _ => String::new(),
        };
        if !query.is_empty() {
            self.apply_discover_filter(&query);
        }
    }

    /// Current cursor position.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Apply discover filter based on search query.
    fn apply_discover_filter(&mut self, query: &str) {
        let filtered = if query.is_empty() {
            self.discover_entries.clone()
        } else {
            let q = query.to_lowercase();
            self.discover_entries
                .iter()
                .filter(|e| {
                    e.name.to_lowercase().contains(&q)
                        || e.description.to_lowercase().contains(&q)
                        || e.source.to_lowercase().contains(&q)
                })
                .cloned()
                .collect()
        };
        if let PluginView::Discover {
            results, cursor, ..
        } = &mut self.view
        {
            *results = filtered;
            if *cursor > results.len().saturating_sub(1) {
                *cursor = results.len().saturating_sub(1);
            }
        }
    }

    // -- Key handlers per mode --

    /// Handle key in Installed list view.
    fn handle_installed_key(&mut self, key: tui_textarea::Key, mods: bool) -> Vec<PanelEffect> {
        match key {
            tui_textarea::Key::Esc => vec![PanelEffect::Close],
            tui_textarea::Key::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                vec![]
            }
            tui_textarea::Key::Down => {
                if !self.entries.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.entries.len() - 1);
                }
                vec![]
            }
            tui_textarea::Key::Enter => {
                if self.entries.get(self.cursor).is_some() {
                    let idx = self.cursor;
                    self.view = PluginView::Detail {
                        index: idx,
                        is_discover: false,
                    };
                    self.detail_cursor = 0;
                }
                vec![]
            }
            tui_textarea::Key::Char('d') if !mods => {
                if let Some(entry) = self.entries.get(self.cursor) {
                    self.confirm_delete = Some(entry.name.clone());
                }
                vec![]
            }
            tui_textarea::Key::Char('s') if !mods => {
                self.view = PluginView::Discover {
                    search_query: String::new(),
                    search_focused: true,
                    results: self.discover_entries.clone(),
                    cursor: 0,
                };
                self.detail_cursor = 0;
                vec![PanelEffect::SendToAcp {
                    event: "query_discover_plugins".to_string(),
                    data: serde_json::Value::Null,
                }]
            }
            _ => vec![],
        }
    }

    /// Handle key in confirm-delete mode.
    fn handle_confirm_delete_key(&mut self, key: tui_textarea::Key) -> Vec<PanelEffect> {
        match key {
            tui_textarea::Key::Enter => {
                let name = self.confirm_delete.take().unwrap_or_default();
                self.confirm_delete = None;
                vec![PanelEffect::SendToAcp {
                    event: "plugin_uninstall".to_string(),
                    data: serde_json::json!({ "name": name }),
                }]
            }
            tui_textarea::Key::Esc
            | tui_textarea::Key::Char('n')
            | tui_textarea::Key::Char('q') => {
                self.confirm_delete = None;
                vec![]
            }
            _ => vec![],
        }
    }

    /// Handle key in Detail view.
    fn handle_detail_key(&mut self, key: tui_textarea::Key) -> Vec<PanelEffect> {
        match key {
            tui_textarea::Key::Esc => {
                self.view = PluginView::Installed;
                self.detail_cursor = 0;
                vec![]
            }
            tui_textarea::Key::Up => {
                self.detail_cursor = self.detail_cursor.saturating_sub(1);
                vec![]
            }
            tui_textarea::Key::Down => {
                let max = if self.view_is_discover() {
                    DiscoverDetailAction::ALL.len().saturating_sub(1)
                } else {
                    DetailAction::ALL.len().saturating_sub(1)
                };
                self.detail_cursor = (self.detail_cursor + 1).min(max);
                vec![]
            }
            tui_textarea::Key::Enter => self.execute_detail_action(),
            _ => vec![],
        }
    }

    /// Whether the current detail view is for a discover entry.
    fn view_is_discover(&self) -> bool {
        matches!(
            self.view,
            PluginView::Detail {
                is_discover: true,
                ..
            }
        )
    }

    /// Execute the currently selected detail action.
    fn execute_detail_action(&mut self) -> Vec<PanelEffect> {
        if self.view_is_discover() {
            let action = DiscoverDetailAction::ALL.get(self.detail_cursor);
            match action {
                Some(DiscoverDetailAction::Install) => {
                    let name = self.get_current_detail_name().unwrap_or_default();
                    vec![PanelEffect::SendToAcp {
                        event: "plugin_install".to_string(),
                        data: serde_json::json!({ "name": name }),
                    }]
                }
                Some(DiscoverDetailAction::BackToList) | None => {
                    self.view = PluginView::Discover {
                        search_query: String::new(),
                        search_focused: false,
                        results: self.discover_entries.clone(),
                        cursor: 0,
                    };
                    self.detail_cursor = 0;
                    vec![]
                }
            }
        } else {
            let action = DetailAction::ALL.get(self.detail_cursor);
            let entry = self.entries.get(self.cursor).cloned();
            match action {
                Some(DetailAction::ToggleEnabled) => {
                    let name = entry.map(|e| e.name).unwrap_or_default();
                    vec![PanelEffect::SendToAcp {
                        event: "plugin_toggle".to_string(),
                        data: serde_json::json!({ "name": name }),
                    }]
                }
                Some(DetailAction::Uninstall) => {
                    let name = entry.map(|e| e.name).unwrap_or_default();
                    self.confirm_delete = Some(name.clone());
                    vec![]
                }
                Some(DetailAction::BackToList) | None => {
                    self.view = PluginView::Installed;
                    self.detail_cursor = 0;
                    vec![]
                }
            }
        }
    }

    /// Get the name of the entry in the current detail view.
    fn get_current_detail_name(&self) -> Option<String> {
        match &self.view {
            PluginView::Detail {
                index,
                is_discover: true,
            } => self.discover_entries.get(*index).map(|e| e.name.clone()),
            PluginView::Detail {
                index,
                is_discover: false,
            } => self.entries.get(*index).map(|e| e.name.clone()),
            _ => None,
        }
    }

    /// Handle key in Discover view.
    fn handle_discover_key(&mut self, key: tui_textarea::Key, ctrl: bool) -> Vec<PanelEffect> {
        let (search_query, search_focused, results, cursor) = match &mut self.view {
            PluginView::Discover {
                search_query,
                search_focused,
                results,
                cursor,
            } => (search_query, search_focused, results, cursor),
            _ => return vec![],
        };

        match key {
            tui_textarea::Key::Esc => {
                self.view = PluginView::Installed;
                self.detail_cursor = 0;
                vec![]
            }
            tui_textarea::Key::Tab => {
                *search_focused = !*search_focused;
                vec![]
            }
            tui_textarea::Key::Enter => {
                if *search_focused {
                    *search_focused = false;
                    vec![]
                } else if let Some(_entry) = results.get(*cursor) {
                    let idx = *cursor;
                    self.view = PluginView::Detail {
                        index: idx,
                        is_discover: true,
                    };
                    self.detail_cursor = 0;
                    vec![]
                } else {
                    vec![]
                }
            }
            tui_textarea::Key::Up => {
                if !*search_focused {
                    *cursor = cursor.saturating_sub(1);
                }
                vec![]
            }
            tui_textarea::Key::Down => {
                if !*search_focused && !results.is_empty() {
                    *cursor = (*cursor + 1).min(results.len() - 1);
                }
                vec![]
            }
            tui_textarea::Key::Char(_) if ctrl => vec![],
            tui_textarea::Key::Char(c) if *search_focused => {
                self.search_field.insert_char(c);
                *search_query = self.search_field.value().to_string();
                let q = search_query.clone();
                self.apply_discover_filter(&q);
                vec![]
            }
            tui_textarea::Key::Backspace if *search_focused => {
                self.search_field.delete_backward();
                *search_query = self.search_field.value().to_string();
                let q = search_query.clone();
                self.apply_discover_filter(&q);
                vec![]
            }
            tui_textarea::Key::Delete if *search_focused => {
                self.search_field.delete_forward();
                *search_query = self.search_field.value().to_string();
                let q = search_query.clone();
                self.apply_discover_filter(&q);
                vec![]
            }
            tui_textarea::Key::Left if *search_focused => {
                self.search_field.move_left();
                vec![]
            }
            tui_textarea::Key::Right if *search_focused => {
                self.search_field.move_right();
                vec![]
            }
            tui_textarea::Key::Home if *search_focused => {
                self.search_field.move_home();
                vec![]
            }
            tui_textarea::Key::End if *search_focused => {
                self.search_field.move_end();
                vec![]
            }
            _ => vec![],
        }
    }

    // -- Render helpers --

    /// Truncate a string to fit within `max_width` display columns.
    fn truncate_display(s: &str, max_width: usize) -> String {
        if UnicodeWidthStr::width(s) <= max_width {
            s.to_string()
        } else {
            let mut width = 0;
            let end = s
                .char_indices()
                .find(|&(_, c)| {
                    width += UnicodeWidthChar::width(c).unwrap_or(0);
                    width > max_width.saturating_sub(1)
                })
                .map(|(i, _)| i)
                .unwrap_or(s.len());
            format!("{}...", &s[..end])
        }
    }

    /// Build a key-value line for detail view.
    fn detail_kv_line(key: &str, value: &str) -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {}: ", key), Style::default().fg(theme::MUTED)),
            Span::styled(value.to_string(), Style::default().fg(theme::TEXT)),
        ])
    }

    /// Render the Installed view.
    fn render_installed(&mut self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line<'_>> = Vec::new();
        let mut cursor_row: u16 = 0;

        // Tab bar: Installed | Discover
        let tab_labels: Vec<Span<'_>> = vec![
            Span::styled(
                " Installed ",
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::THINKING)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Discover ", Style::default().fg(theme::MUTED)),
        ];
        lines.push(Line::from(tab_labels));
        lines.push(Line::from(""));

        let table_header_height: u16 = 3; // header + blank

        if self.entries.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No plugins installed",
                Style::default().fg(theme::MUTED),
            )));
        } else {
            // Table header
            lines.push(Line::from(vec![
                Span::styled(
                    "  Plugin",
                    Style::default()
                        .fg(theme::MUTED)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "                  Version  Status  Source",
                    Style::default().fg(theme::MUTED),
                ),
            ]));
            lines.push(Line::from(""));

            for (row_idx, entry) in self.entries.iter().enumerate() {
                let is_cursor = row_idx == self.cursor;
                if is_cursor {
                    cursor_row = table_header_height + row_idx as u16;
                }
                let cursor_char = if is_cursor { "\u{276F} " } else { "  " };

                let (status_icon, status_style) = if entry.enabled {
                    ("\u{2714} ", Style::default().fg(theme::SAGE))
                } else {
                    ("  ", Style::default().fg(theme::MUTED))
                };

                let name_style = if is_cursor {
                    Style::default()
                        .fg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let name_width = 18;
                let display_name = Self::truncate_display(&entry.name, name_width);
                let name_padding = " ".repeat(
                    name_width.saturating_sub(UnicodeWidthStr::width(display_name.as_str())),
                );

                let update_badge = if entry.has_update { " (update)" } else { "" };

                lines.push(Line::from(vec![
                    Span::styled(
                        cursor_char.to_string(),
                        Style::default().fg(theme::THINKING),
                    ),
                    Span::styled(display_name, name_style),
                    Span::styled(name_padding, Style::default()),
                    Span::styled(
                        format!("{}  ", entry.version),
                        Style::default().fg(theme::MUTED),
                    ),
                    Span::styled(status_icon.to_string(), status_style),
                    Span::styled("  ", Style::default()),
                    Span::styled(entry.source.clone(), Style::default().fg(theme::MUTED)),
                    Span::styled(
                        update_badge.to_string(),
                        if entry.has_update {
                            Style::default().fg(theme::WARNING)
                        } else {
                            Style::default().fg(theme::MUTED)
                        },
                    ),
                ]));
            }
        }

        let inner = BorderedPanel::new(Span::styled(
            " Plugins ",
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let visible_height = inner.height.saturating_sub(1);
        let mut scroll = self.scroll_offset as i16;
        let total_height = lines.len() as i16;
        if visible_height as i16 >= total_height {
            scroll = 0;
        } else {
            let max_scroll = total_height - visible_height as i16;
            if cursor_row as i16 + scroll >= visible_height as i16 {
                scroll = (cursor_row as i16 + scroll) - visible_height as i16 + 1;
            }
            scroll = scroll.max(0).min(max_scroll);
        }
        self.scroll_offset = scroll as u16;

        let visible_lines: Vec<Line<'_>> = lines
            .iter()
            .skip(scroll as usize)
            .take(visible_height as usize)
            .cloned()
            .collect();

        f.render_widget(Paragraph::new(Text::from(visible_lines)), inner);
    }

    /// Render the Detail view for an installed plugin.
    fn render_detail_installed(&self, f: &mut Frame, area: Rect) {
        let (index, _) = match &self.view {
            PluginView::Detail {
                index,
                is_discover: false,
            } => (*index, false),
            _ => return,
        };
        let entry = match self.entries.get(index) {
            Some(e) => e,
            None => return,
        };

        let mut lines: Vec<Line<'_>> = Vec::new();

        // Header
        let header = if entry.source.is_empty() {
            entry.name.clone()
        } else {
            format!("{} @ {}", entry.name, entry.source)
        };
        lines.push(Line::from(Span::styled(
            format!("  {}", header),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        )));

        // Fields
        lines.push(Self::detail_kv_line("Version:", &entry.version));
        lines.push(Self::detail_kv_line("Source:", &entry.source));

        // Description
        if !entry.description.is_empty() {
            lines.push(Line::from(""));
            for desc_line in entry.description.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", desc_line),
                    Style::default().fg(theme::MUTED),
                )));
            }
        }

        // Status
        lines.push(Line::from(""));
        let (status_icon, status_style, status_text) = if entry.enabled {
            ("\u{2714}", Style::default().fg(theme::SAGE), "Enabled")
        } else {
            ("\u{25CB}", Style::default().fg(theme::MUTED), "Disabled")
        };
        lines.push(Line::from(vec![
            Span::styled("  Status: ".to_string(), Style::default().fg(theme::MUTED)),
            Span::styled(format!("{} {}", status_icon, status_text), status_style),
        ]));

        if entry.has_update {
            lines.push(Line::from(vec![
                Span::styled("  ".to_string(), Style::default()),
                Span::styled("Update available", Style::default().fg(theme::WARNING)),
            ]));
        }

        // Action menu
        lines.push(Line::from(""));
        lines.push(Line::from(""));

        for (i, action) in DetailAction::ALL.iter().enumerate() {
            let is_cursor = i == self.detail_cursor;
            let cursor_char = if is_cursor { "\u{276F} " } else { "  " };
            let label = action.label(entry.enabled);
            let style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    cursor_char.to_string(),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(label.to_string(), style),
            ]));
        }

        let inner = BorderedPanel::new(Span::styled(
            " Plugins ",
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    /// Render the Detail view for a discover plugin.
    fn render_detail_discover(&self, f: &mut Frame, area: Rect) {
        let index = match &self.view {
            PluginView::Detail {
                index,
                is_discover: true,
            } => *index,
            _ => return,
        };
        let entry = match self.discover_entries.get(index) {
            Some(e) => e,
            None => return,
        };

        let mut lines: Vec<Line<'_>> = Vec::new();

        // Header
        lines.push(Line::from(Span::styled(
            format!("  {}", entry.name),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        )));

        lines.push(Self::detail_kv_line("Version:", &entry.version));
        lines.push(Self::detail_kv_line("Source:", &entry.source));

        if !entry.description.is_empty() {
            lines.push(Line::from(""));
            for desc_line in entry.description.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", desc_line),
                    Style::default().fg(theme::MUTED),
                )));
            }
        }

        // Action menu
        lines.push(Line::from(""));
        lines.push(Line::from(""));

        for (i, action) in DiscoverDetailAction::ALL.iter().enumerate() {
            let is_cursor = i == self.detail_cursor;
            let cursor_char = if is_cursor { "\u{276F} " } else { "  " };
            let style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    cursor_char.to_string(),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(action.label().to_string(), style),
            ]));
        }

        let inner = BorderedPanel::new(Span::styled(
            " Plugins ",
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    /// Render the Discover view.
    fn render_discover(&self, f: &mut Frame, area: Rect) {
        let (search_query, search_focused, results, cursor) = match &self.view {
            PluginView::Discover {
                search_query,
                search_focused,
                results,
                cursor,
            } => (search_query, search_focused, results, *cursor),
            _ => return,
        };

        let inner = BorderedPanel::new(Span::styled(
            " Plugins ",
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        // Tab bar
        let tab_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 2,
        };
        let tab_labels = vec![
            Span::styled(" Installed ", Style::default().fg(theme::MUTED)),
            Span::styled(
                " Discover ",
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::THINKING)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        f.render_widget(
            Paragraph::new(vec![Line::from(tab_labels), Line::from("")]),
            tab_area,
        );

        // Search box area
        let search_border_area = Rect {
            x: inner.x + 1,
            y: inner.y + 2,
            width: inner.width.saturating_sub(2),
            height: 3,
        };

        let search_border = if *search_focused {
            Style::default().fg(theme::BORDER_ACTIVE)
        } else {
            Style::default().fg(theme::BORDER)
        };
        let search_inner = BorderedPanel::new(Span::styled(
            " Search ",
            Style::default().fg(theme::THINKING),
        ))
        .border_style(search_border)
        .render(f, search_border_area);

        let display_query = if *search_focused {
            let (before, after) = search_query.split_at(self.search_field.cursor);
            format!("{}|{}", before, after)
        } else {
            search_query.clone()
        };
        if display_query.is_empty() && !search_focused {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "Type to search plugins...",
                    Style::default().fg(theme::DIM),
                )),
                search_inner,
            );
        } else {
            f.render_widget(
                Paragraph::new(Span::styled(
                    display_query,
                    Style::default().fg(theme::TEXT),
                )),
                search_inner,
            );
        }

        // Plugin list
        let list_area = Rect {
            x: inner.x,
            y: inner.y + 6,
            width: inner.width,
            height: inner.height.saturating_sub(6),
        };

        let mut lines: Vec<Line<'_>> = Vec::new();
        let max_name_width = list_area.width.saturating_sub(8) as usize;

        if results.is_empty() {
            let msg = if search_query.is_empty() {
                "  No plugins available (Marketplace data pending P3 Integration)"
            } else {
                "  No matching plugins"
            };
            lines.push(Line::from(Span::styled(
                msg,
                Style::default().fg(theme::MUTED),
            )));
        } else {
            for (i, entry) in results.iter().enumerate() {
                let is_cursor = i == cursor;
                let cursor_char = if is_cursor { "\u{276F} " } else { "  " };

                let name_style = if is_cursor {
                    Style::default()
                        .fg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let display_name = Self::truncate_display(&entry.name, max_name_width);

                lines.push(Line::from(vec![
                    Span::styled(
                        cursor_char.to_string(),
                        Style::default().fg(theme::THINKING),
                    ),
                    Span::styled(display_name, name_style),
                    Span::styled(
                        format!("  {}", entry.version),
                        Style::default().fg(theme::MUTED),
                    ),
                ]));

                let desc_width = list_area.width.saturating_sub(6) as usize;
                let desc = Self::truncate_display(&entry.description, desc_width);
                if !desc.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("     ", Style::default()),
                        Span::styled(desc, Style::default().fg(theme::MUTED)),
                    ]));
                } else {
                    lines.push(Line::from(""));
                }
            }
        }

        f.render_widget(Paragraph::new(Text::from(lines)), list_area);
    }
}

// ---------------------------------------------------------------------------
// PanelState implementation
// ---------------------------------------------------------------------------

impl std::fmt::Debug for PluginPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginPanel")
            .field("entries_len", &self.entries.len())
            .field("cursor", &self.cursor)
            .field("scroll_offset", &self.scroll_offset)
            .field("view", &self.view)
            .field("confirm_delete", &self.confirm_delete)
            .field("detail_cursor", &self.detail_cursor)
            .field("search_field", &self.search_field)
            .field("discover_entries_len", &self.discover_entries.len())
            .finish()
    }
}

impl PanelState for PluginPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Plugin
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelReadContext) {
        if self.confirm_delete.is_some() {
            self.render_confirm_delete(f, area);
            return;
        }
        match &self.view {
            PluginView::Installed => self.render_installed(f, area),
            PluginView::Detail {
                is_discover: false, ..
            } => self.render_detail_installed(f, area),
            PluginView::Detail {
                is_discover: true, ..
            } => self.render_detail_discover(f, area),
            PluginView::Discover { .. } => self.render_discover(f, area),
        }
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        let key = input.key;
        let ctrl = input.ctrl;

        // 1. Confirm-delete mode takes priority
        if self.confirm_delete.is_some() {
            return self.handle_confirm_delete_key(key);
        }

        // 2. Detail view
        if self.view.is_detail() {
            return self.handle_detail_key(key);
        }

        // 3. Discover view
        if self.view.is_discover() {
            return self.handle_discover_key(key, ctrl);
        }

        // 4. Installed list (default)
        self.handle_installed_key(key, ctrl)
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.handle_scroll(1, _ctx),
            MouseEventKind::ScrollUp => self.handle_scroll(-1, _ctx),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.view.is_installed() && !self.entries.is_empty() {
                    // Calculate from area: border(1) + tab(1) + blank(1) + header(1) + blank(1) = 5
                    let table_start = area.y + 5;
                    if mouse.row >= table_start {
                        let clicked_row = (mouse.row - table_start) as usize;
                        if clicked_row < self.entries.len() {
                            self.cursor = clicked_row;
                        }
                    }
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn handle_scroll(&mut self, lines: i16, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        let new_offset = (self.scroll_offset as i16 + lines).max(0) as u16;
        self.scroll_offset = new_offset;
        vec![]
    }

    fn handle_paste(&mut self, text: &str, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        if let PluginView::Discover {
            search_focused: true,
            ..
        } = &self.view
        {
            for c in text.chars() {
                self.search_field.insert_char(c);
            }
            let query = self.search_field.value().to_string();
            self.apply_discover_filter(&query);
            if let PluginView::Discover { search_query, .. } = &mut self.view {
                *search_query = query;
            }
        }
        vec![]
    }

    fn desired_height(&self, screen_h: u16, _screen_w: u16) -> u16 {
        screen_h * 70 / 100
    }

    fn status_bar_hints(&self, _lc: &LcRegistry) -> Vec<(String, String)> {
        if self.confirm_delete.is_some() {
            return vec![
                ("Enter".to_string(), "Confirm uninstall".to_string()),
                ("Other".to_string(), "Cancel".to_string()),
            ];
        }
        match &self.view {
            PluginView::Installed => vec![
                ("Up/Down".to_string(), "Move".to_string()),
                ("Enter".to_string(), "Detail".to_string()),
                ("d".to_string(), "Uninstall".to_string()),
                ("s".to_string(), "Discover".to_string()),
                ("Esc".to_string(), "Close".to_string()),
            ],
            PluginView::Detail { .. } => vec![
                ("Up/Down".to_string(), "Move".to_string()),
                ("Enter".to_string(), "Execute".to_string()),
                ("Esc".to_string(), "Back".to_string()),
            ],
            PluginView::Discover { search_focused, .. } => {
                if *search_focused {
                    vec![
                        ("Esc".to_string(), "Back".to_string()),
                        ("Tab".to_string(), "List".to_string()),
                        ("Enter".to_string(), "List".to_string()),
                        ("Backspace".to_string(), "Delete".to_string()),
                    ]
                } else {
                    vec![
                        ("Up/Down".to_string(), "Select".to_string()),
                        ("Enter".to_string(), "Detail".to_string()),
                        ("Tab".to_string(), "Search".to_string()),
                        ("Esc".to_string(), "Back".to_string()),
                    ]
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private: confirm-delete overlay rendering
// ---------------------------------------------------------------------------

impl PluginPanel {
    fn render_confirm_delete(&self, f: &mut Frame, area: Rect) {
        let name = self.confirm_delete.as_deref().unwrap_or("?");
        let lines: Vec<Line<'_>> = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "  Confirm uninstall ".to_string(),
                    Style::default().fg(theme::TEXT),
                ),
                Span::styled(
                    name.to_string(),
                    Style::default()
                        .fg(theme::THINKING)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ?", Style::default().fg(theme::TEXT)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Press ", Style::default().fg(theme::MUTED)),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to confirm, ", Style::default().fg(theme::MUTED)),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to cancel", Style::default().fg(theme::MUTED)),
            ]),
        ];

        let inner = BorderedPanel::new(Span::styled(
            " Plugins ",
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

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
            key: tui_textarea::Key::Esc,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn up_input() -> Input {
        Input {
            key: tui_textarea::Key::Up,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn down_input() -> Input {
        Input {
            key: tui_textarea::Key::Down,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn enter_input() -> Input {
        Input {
            key: tui_textarea::Key::Enter,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn tab_input() -> Input {
        Input {
            key: tui_textarea::Key::Tab,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn char_input(c: char) -> Input {
        Input {
            key: tui_textarea::Key::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn backspace_input() -> Input {
        Input {
            key: tui_textarea::Key::Backspace,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// Construct a test `PluginEntry`.
    fn make_entry(name: &str, version: &str, source: &str, enabled: bool) -> PluginEntry {
        PluginEntry {
            name: name.to_string(),
            version: version.to_string(),
            description: format!("Description for {}", name),
            source: source.to_string(),
            enabled,
            has_update: false,
        }
    }

    #[test]
    fn test_kind_returns_correct_variant() {
        let panel = PluginPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Plugin);
    }

    #[test]
    fn test_esc_close_from_installed() {
        let mut panel = PluginPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_esc_from_detail_returns_to_installed() {
        let entries = vec![make_entry(
            "test-plugin",
            "1.0.0",
            "marketplace:default",
            true,
        )];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();

        // Drill into detail
        panel.handle_key(enter_input(), &ctx);
        assert!(panel.view.is_detail());

        // Esc returns to installed
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 0);
        assert!(panel.view.is_installed());
    }

    #[test]
    fn test_enter_navigates_to_detail() {
        let entries = vec![
            make_entry("alpha", "1.0.0", "marketplace:default", true),
            make_entry("beta", "2.0.0", "local", false),
        ];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();

        // Enter on cursor=0 (alpha)
        let effects = panel.handle_key(enter_input(), &ctx);
        assert_eq!(effects.len(), 0);
        match &panel.view {
            PluginView::Detail { index, is_discover } => {
                assert_eq!(*index, 0);
                assert!(!is_discover);
            }
            _ => panic!("expected Detail view"),
        }
    }

    #[test]
    fn test_s_enters_discover_mode() {
        let mut panel = PluginPanel::empty();
        let ctx = make_ctx();

        let effects = panel.handle_key(char_input('s'), &ctx);
        assert!(panel.view.is_discover());
        assert!(effects.iter().any(|e| matches!(
            e,
            PanelEffect::SendToAcp { event, .. } if event == "query_discover_plugins"
        )));
    }

    #[test]
    fn test_discover_search_input() {
        let mut panel = PluginPanel::empty();
        let ctx = make_ctx();

        // Enter discover mode
        panel.handle_key(char_input('s'), &ctx);
        assert!(panel.view.is_discover());

        // Type search query
        panel.handle_key(char_input('h'), &ctx);
        panel.handle_key(char_input('e'), &ctx);
        panel.handle_key(char_input('l'), &ctx);

        let query = match &panel.view {
            PluginView::Discover { search_query, .. } => search_query.clone(),
            _ => panic!("expected Discover view"),
        };
        assert_eq!(query, "hel");

        // Tab to switch focus away from search
        panel.handle_key(tab_input(), &ctx);
        let focused = match &panel.view {
            PluginView::Discover { search_focused, .. } => *search_focused,
            _ => panic!("expected Discover view"),
        };
        assert!(!focused);

        // Tab back to search
        panel.handle_key(tab_input(), &ctx);
        let focused = match &panel.view {
            PluginView::Discover { search_focused, .. } => *search_focused,
            _ => panic!("expected Discover view"),
        };
        assert!(focused);

        // Backspace deletes
        panel.handle_key(backspace_input(), &ctx);
        let query = match &panel.view {
            PluginView::Discover { search_query, .. } => search_query.clone(),
            _ => panic!("expected Discover view"),
        };
        assert_eq!(query, "he");
    }

    #[test]
    fn test_delete_flow() {
        let entries = vec![make_entry(
            "test-plugin",
            "1.0.0",
            "marketplace:default",
            true,
        )];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();

        // 'd' enters confirm mode
        panel.handle_key(char_input('d'), &ctx);
        assert!(panel.confirm_delete.is_some());
        assert_eq!(panel.confirm_delete.as_deref(), Some("test-plugin"));

        // Enter confirms -> produces SendToAcp
        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(panel.confirm_delete.is_none());
        assert!(effects.iter().any(|e| matches!(
            e,
            PanelEffect::SendToAcp { event, .. } if event == "plugin_uninstall"
        )));

        // Test cancel with Esc
        panel.handle_key(char_input('d'), &ctx);
        assert!(panel.confirm_delete.is_some());
        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(panel.confirm_delete.is_none());
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_render_does_not_panic_installed() {
        let entries = vec![
            make_entry("plugin-alpha", "1.0.0", "marketplace:default", true),
            make_entry("plugin-beta", "2.1.0", "local", false),
        ];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_detail() {
        let entries = vec![make_entry(
            "test-plugin",
            "1.0.0",
            "marketplace:default",
            true,
        )];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();

        // Drill into detail
        panel.handle_key(enter_input(), &ctx);
        assert!(panel.view.is_detail());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_discover() {
        let mut panel = PluginPanel::empty();
        let ctx = make_ctx();

        // Enter discover mode
        panel.handle_key(char_input('s'), &ctx);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_arrow_keys_move_cursor() {
        let entries = vec![
            make_entry("alpha", "1.0.0", "marketplace:default", true),
            make_entry("beta", "2.0.0", "local", false),
            make_entry("gamma", "0.5.0", "git:url", true),
        ];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();

        assert_eq!(panel.cursor(), 0);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 1);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 2);

        // Clamp at end
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 2);

        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 1);

        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 0);

        // Clamp at start
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 0);
    }

    #[test]
    fn test_arrow_keys_move_cursor_in_detail() {
        let entries = vec![make_entry("test", "1.0.0", "source", true)];
        let mut panel = PluginPanel::new(entries);
        let ctx = make_ctx();

        // Drill into detail
        panel.handle_key(enter_input(), &ctx);
        assert_eq!(panel.detail_cursor, 0);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.detail_cursor, 1);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.detail_cursor, 2);

        // Clamp at end (3 actions: 0, 1, 2)
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.detail_cursor, 2);

        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.detail_cursor, 1);
    }

    #[test]
    fn test_status_bar_hints_installed() {
        let panel = PluginPanel::empty();
        let lc = LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 5); // arrows, enter, d, s, esc
    }

    #[test]
    fn test_status_bar_hints_detail() {
        let entries = vec![make_entry("test", "1.0.0", "source", true)];
        let mut panel = PluginPanel::new(entries);
        panel.handle_key(enter_input(), &make_ctx());
        let lc = LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 3); // arrows, enter, esc
    }

    #[test]
    fn test_status_bar_hints_confirm_delete() {
        let entries = vec![make_entry("test", "1.0.0", "source", true)];
        let mut panel = PluginPanel::new(entries);
        panel.handle_key(char_input('d'), &make_ctx());
        let lc = LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2); // enter, other
    }

    #[test]
    fn test_esc_from_discover_returns_to_installed() {
        let mut panel = PluginPanel::empty();
        let ctx = make_ctx();

        // Enter discover
        panel.handle_key(char_input('s'), &ctx);
        assert!(panel.view.is_discover());

        // Esc returns to installed
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 0);
        assert!(panel.view.is_installed());
    }

    #[test]
    fn test_empty_panel_render_no_panic() {
        let mut panel = PluginPanel::empty();
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }
}
