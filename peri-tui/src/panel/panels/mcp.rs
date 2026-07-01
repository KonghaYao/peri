//! v2 McpPanel -- MCP server management panel (PanelState trait implementation).
//!
//! Displays MCP servers in two views:
//!   - **ServerList**: grouped by source (project / global), with status icons.
//!   - **ServerDetail**: server metadata + action menu (ViewTools, Reconnect, etc.).
//!
//! Navigation: Up/Down to move cursor; Enter to drill-in or execute action;
//! Esc to go back (detail -> list) or close (list). Ctrl+D to delete server.
//!
//! Data is provided as `Vec<McpServerEntry>` (local DTOs). No direct dependency
//! on `peri_middlewares::mcp` runtime types.
//!
//! **Data source**: `app.services.mcp_pool.server_infos()` via `from_app()` — P3 Integration 已完成。

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_textarea::Input;

use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// Local DTOs (no peri_middlewares::mcp dependency)
// ---------------------------------------------------------------------------

/// Display-friendly MCP server entry.
///
/// Fields mirror `peri_middlewares::mcp::ServerInfo` at the rendering layer.
/// P3 Integration: populated from `McpClientPool::server_infos()` via `from_app()`.
#[derive(Debug, Clone)]
pub struct McpServerEntry {
    /// Server display name.
    pub name: String,
    /// Connection status string ("connected" / "disabled" / "error" / "uninitialized" / "offline").
    pub status: String,
    /// Auth status string ("authorized" / "needs_auth" / "none").
    pub auth_status: String,
    /// Config source string ("project" / "global" / "plugin").
    pub source: String,
    /// Transport type ("stdio" / "http" / etc.).
    pub transport_type: String,
    /// Server URL (for HTTP transports).
    pub url: Option<String>,
    /// Number of registered tools.
    pub tool_count: usize,
    /// Number of registered resources.
    pub resource_count: usize,
}

/// Detail-view action menu item.
#[derive(Debug, Clone, PartialEq)]
pub enum DetailAction {
    /// Toggle tool list visibility.
    ViewTools,
    /// Re-authenticate OAuth.
    ReAuthenticate,
    /// Clear OAuth credentials.
    ClearAuth,
    /// Reconnect the server.
    Reconnect,
    /// Disable a connected server.
    Disable,
    /// Enable a disabled server.
    Enable,
}

impl DetailAction {
    /// Localised label key for each action.
    fn label_key(&self) -> &'static str {
        match self {
            Self::ViewTools => "mcp-action-view-tools",
            Self::ReAuthenticate => "mcp-action-reauthenticate",
            Self::ClearAuth => "mcp-action-clear-auth",
            Self::Reconnect => "mcp-action-reconnect",
            Self::Disable => "mcp-action-disable",
            Self::Enable => "mcp-action-enable",
        }
    }
}

/// Panel view mode.
#[derive(Debug, Clone)]
pub enum McpView {
    /// Flat server list (grouped by source during render).
    ServerList,
    /// Server detail with action menu.
    ServerDetail {
        /// Name of the selected server.
        server_name: String,
        /// Available action menu items.
        actions: Vec<DetailAction>,
        /// Whether the tool list is expanded.
        show_tools: bool,
    },
}

impl McpView {
    fn is_server_list(&self) -> bool {
        matches!(self, McpView::ServerList)
    }

    fn action_count(&self) -> usize {
        match self {
            McpView::ServerList => 0,
            McpView::ServerDetail { actions, .. } => actions.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// McpPanel
// ---------------------------------------------------------------------------

/// v2 MCP server management panel.
///
/// Two-view panel: ServerList (flat list grouped by source) and ServerDetail
/// (metadata + action menu). All data is local DTOs; no `peri_middlewares::mcp`
/// dependency.
#[derive(Debug)]
pub struct McpPanel {
    /// Server entries (empty until data is injected).
    servers: Vec<McpServerEntry>,
    /// Current view mode.
    view: McpView,
    /// Cursor for ServerList (index into `servers`).
    cursor: usize,
    /// Cursor for ServerDetail (index into `actions`).
    detail_cursor: usize,
    /// Vertical scroll offset (in lines, 0-based).
    scroll_offset: u16,
    /// Confirm-delete state: `Some(server_name)` when awaiting confirmation.
    confirm_delete: Option<String>,
}

impl McpPanel {
    /// Create an empty panel (no servers loaded yet).
    ///
    /// Used by the registry factory. Servers can be populated later via
    /// `set_servers()` when ACP query results arrive.
    pub fn empty() -> Self {
        Self {
            servers: Vec::new(),
            view: McpView::ServerList,
            cursor: 0,
            detail_cursor: 0,
            scroll_offset: 0,
            confirm_delete: None,
        }
    }

    /// Construct a panel from the live `App` state.
    ///
    /// Reads MCP server info from `app.services.mcp_pool` (if available) and
    /// converts `ServerInfo` runtime types to panel-local `McpServerEntry` DTOs.
    pub fn from_app(app: &crate::app::App) -> Self {
        let servers = Self::servers_from_app(app);
        if servers.is_empty() {
            Self::empty()
        } else {
            Self::new(servers)
        }
    }

    /// Pull fresh MCP server info from the live pool, convert to DTOs.
    ///
    /// Cron #30: extracted from `from_app` so `refresh` can reuse the
    /// conversion without duplicating the ServerInfo → McpServerEntry mapping.
    fn servers_from_app(app: &crate::app::App) -> Vec<McpServerEntry> {
        match &app.services.mcp_pool {
            Some(pool) => {
                use peri_middlewares::mcp::{ClientStatus, ConfigSource, OAuthStatus};
                pool.server_infos()
                    .into_iter()
                    .map(|s| McpServerEntry {
                        name: s.name,
                        status: match s.status {
                            ClientStatus::Connected => "connected".to_string(),
                            ClientStatus::Failed(_) => "error".to_string(),
                            ClientStatus::Disconnected => "offline".to_string(),
                            ClientStatus::Disabled => "disabled".to_string(),
                            ClientStatus::Uninitialized => "uninitialized".to_string(),
                        },
                        auth_status: match s.oauth_status {
                            OAuthStatus::None => "none".to_string(),
                            OAuthStatus::Authorized => "authorized".to_string(),
                            OAuthStatus::NeedsAuthorization => "needs_auth".to_string(),
                        },
                        source: match s.source {
                            Some(ConfigSource::Project(_)) => "project".to_string(),
                            Some(ConfigSource::Global(_)) => "global".to_string(),
                            Some(ConfigSource::Plugin) => "plugin".to_string(),
                            None => "unknown".to_string(),
                        },
                        transport_type: s.transport_type,
                        url: s.url,
                        tool_count: s.tool_count,
                        resource_count: s.resource_count,
                    })
                    .collect()
            }
            None => Vec::new(),
        }
    }

    /// Create a panel from a list of `McpServerEntry`.
    ///
    /// Servers are sorted: project sources first, then by name.
    pub fn new(servers: Vec<McpServerEntry>) -> Self {
        let mut sorted = servers;
        sorted.sort_by(|a, b| {
            let a_is_project = a.source == "project";
            let b_is_project = b.source == "project";
            b_is_project
                .cmp(&a_is_project)
                .then_with(|| a.name.cmp(&b.name))
        });
        Self {
            servers: sorted,
            view: McpView::ServerList,
            cursor: 0,
            detail_cursor: 0,
            scroll_offset: 0,
            confirm_delete: None,
        }
    }

    /// Replace servers data (e.g. after ACP query results arrive).
    pub fn set_servers(&mut self, servers: Vec<McpServerEntry>) {
        let mut sorted = servers;
        sorted.sort_by(|a, b| {
            let a_is_project = a.source == "project";
            let b_is_project = b.source == "project";
            b_is_project
                .cmp(&a_is_project)
                .then_with(|| a.name.cmp(&b.name))
        });
        self.servers = sorted;
        self.cursor = 0;
        self.scroll_offset = 0;
        // If in detail view and the server no longer exists, go back to list.
        if let McpView::ServerDetail { server_name, .. } = &self.view {
            if !self.servers.iter().any(|s| &s.name == server_name) {
                self.view = McpView::ServerList;
                self.detail_cursor = 0;
            }
        }
    }

    /// Current cursor position (list cursor).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the list cursor by `delta`. Clamps to valid range.
    fn move_list_cursor(&mut self, delta: i32) {
        let len = self.servers.len();
        if len == 0 {
            return;
        }
        let new = (self.cursor as i32 + delta).clamp(0, (len - 1) as i32) as usize;
        self.cursor = new;
    }

    /// Move the detail action cursor by `delta`. Clamps to valid range.
    fn move_detail_cursor(&mut self, delta: i32) {
        let max = self.view.action_count().saturating_sub(1);
        let new = (self.detail_cursor as i32 + delta).clamp(0, max as i32) as usize;
        self.detail_cursor = new;
    }

    /// Ensure the list cursor is visible within `visible_lines`.
    fn ensure_list_visible(&mut self, visible_lines: u16) {
        if self.servers.is_empty() {
            return;
        }
        // Header lines: 3 (count + blank + section header) minimum
        let header_lines: u16 = 3;
        let cursor_line = header_lines + (self.cursor as u16);

        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        } else if cursor_line >= self.scroll_offset + visible_lines {
            self.scroll_offset = cursor_line - visible_lines + 1;
        }
    }

    /// Build the action menu for a given server entry.
    fn build_actions(server: &McpServerEntry) -> Vec<DetailAction> {
        let mut actions = vec![DetailAction::ViewTools];
        if server.transport_type == "http" {
            actions.push(DetailAction::ReAuthenticate);
            actions.push(DetailAction::ClearAuth);
        }
        if server.status == "uninitialized" {
            actions = vec![DetailAction::Reconnect];
        } else {
            actions.push(DetailAction::Reconnect);
            if server.status == "disabled" {
                actions.push(DetailAction::Enable);
            } else {
                actions.push(DetailAction::Disable);
            }
        }
        actions
    }

    /// Total content lines for desired_height.
    fn total_content_lines(&self) -> u16 {
        match &self.view {
            McpView::ServerList => {
                let header: u16 = 2; // count + blank
                let group_header: u16 = if self.servers.is_empty() { 0 } else { 1 };
                let entries: u16 = self.servers.len() as u16;
                header + group_header + entries + 1 // +1 trailing blank
            }
            McpView::ServerDetail { actions, .. } => {
                // Status + Auth + URL + Config + Capabilities + Tools + blank + actions
                let meta_lines: u16 = 7;
                let action_lines: u16 = actions.len() as u16;
                meta_lines + action_lines + 1
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: status rendering
// ---------------------------------------------------------------------------

/// Returns (icon, style, localised key) for a server status string.
fn status_render(status: &str) -> (&'static str, Style, &'static str) {
    match status {
        "connected" => (
            "\u{2714}",
            Style::default().fg(theme::SAGE),
            "mcp-status-connected",
        ),
        "disabled" => (
            "\u{25ef}",
            Style::default().fg(theme::MUTED),
            "mcp-status-disabled",
        ),
        "error" => (
            "\u{2717}",
            Style::default().fg(theme::ERROR),
            "mcp-status-error",
        ),
        "uninitialized" => (
            "\u{25ef}",
            Style::default().fg(theme::MUTED),
            "mcp-status-uninitialized",
        ),
        "offline" => (
            "\u{25ef}",
            Style::default().fg(theme::MUTED),
            "mcp-status-offline",
        ),
        "needs_auth" => (
            "\u{25b3}",
            Style::default().fg(theme::WARNING),
            "mcp-status-needs-auth",
        ),
        _ => (
            "\u{25ef}",
            Style::default().fg(theme::MUTED),
            "mcp-status-offline",
        ),
    }
}

/// Generate a key-value detail line.
fn detail_line<'a>(label_width: usize, label: &str, value: &str, value_style: Style) -> Line<'a> {
    let padded = format!("  {:<width$}", label, width = label_width);
    Line::from(vec![
        Span::styled(padded, Style::default().fg(theme::MUTED)),
        Span::styled(value.to_string(), value_style),
    ])
}

// ---------------------------------------------------------------------------
// PanelState impl
// ---------------------------------------------------------------------------

impl PanelState for McpPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Mcp
    }

    /// Cron #30: refresh MCP server list from live pool, preserving
    /// cursor + scroll + view + confirm_delete state.
    ///
    /// Bug: prior to this hook, McpPanel cached `servers` at `from_app`.
    /// MCP connection state is asynchronous — clients connect/disconnect/
    /// reconnect in the background. A user who opened McpPanel to
    /// diagnose a connection problem saw the status from moment of open
    /// and had to Esc+reopen to refresh.
    ///
    /// Fix: pull fresh server info via `servers_from_app`, sort+replace
    /// in place. If in ServerDetail view and the server no longer exists,
    /// fall back to ServerList (mirrors set_servers behavior). Don't
    /// touch cursor/scroll_offset/confirm_delete unless forced by view
    /// fallback.
    fn refresh(&mut self, app: &crate::app::App) {
        let fresh_servers = Self::servers_from_app(app);
        // Sort same way as set_servers (project first, then name)
        let mut sorted = fresh_servers;
        sorted.sort_by(|a, b| {
            let a_is_project = a.source == "project";
            let b_is_project = b.source == "project";
            b_is_project
                .cmp(&a_is_project)
                .then_with(|| a.name.cmp(&b.name))
        });
        self.servers = sorted;
        // Clamp cursor to bounds
        if self.cursor >= self.servers.len() && !self.servers.is_empty() {
            self.cursor = self.servers.len().saturating_sub(1);
        } else if self.servers.is_empty() {
            self.cursor = 0;
        }
        // If in detail view and server is gone, fall back to list
        if let McpView::ServerDetail { server_name, .. } = &self.view {
            if !self.servers.iter().any(|s| &s.name == server_name) {
                self.view = McpView::ServerList;
            }
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;

        match &self.view {
            McpView::ServerList => self.render_server_list(f, area, lc),
            McpView::ServerDetail { .. } => self.render_server_detail(f, area, lc),
        }
    }

    fn handle_key(&mut self, input: Input, ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;

        // confirm_delete mode: any key except Enter cancels
        if self.confirm_delete.is_some() {
            return match input {
                Input {
                    key: Key::Enter, ..
                } => self.do_confirm_delete(ctx),
                _ => {
                    self.confirm_delete = None;
                    vec![]
                }
            };
        }

        match input {
            Input { key: Key::Esc, .. } => {
                if self.view.is_server_list() {
                    vec![PanelEffect::Close]
                } else {
                    self.do_back();
                    vec![]
                }
            }
            Input { key: Key::Up, .. } => {
                if self.view.is_server_list() {
                    self.move_list_cursor(-1);
                    self.ensure_list_visible(16);
                } else {
                    self.move_detail_cursor(-1);
                }
                vec![]
            }
            Input { key: Key::Down, .. } => {
                if self.view.is_server_list() {
                    self.move_list_cursor(1);
                    self.ensure_list_visible(16);
                } else {
                    self.move_detail_cursor(1);
                }
                vec![]
            }
            Input {
                key: Key::Enter, ..
            } => self.do_enter(ctx),
            Input {
                key: Key::Char('d'),
                ctrl: true,
                ..
            } => {
                if self.view.is_server_list() && self.cursor < self.servers.len() {
                    self.confirm_delete = Some(self.servers[self.cursor].name.clone());
                }
                vec![]
            }
            Input {
                key: Key::Char('r'),
                ctrl: true,
                ..
            } => {
                if self.view.is_server_list() && self.cursor < self.servers.len() {
                    let name = self.servers[self.cursor].name.clone();
                    // TODO(P3 Integration): reconnect via SendToAcp
                    let _ = name;
                    vec![PanelEffect::ShowNotification(
                        ctx.lc.tr("mcp-reconnect-not-implemented").to_string(),
                    )]
                } else {
                    vec![]
                }
            }
            Input {
                key: Key::Char('c'),
                ctrl: true,
                ..
            } => vec![],
            // 其他按键：消费但不产生副作用
            _ => vec![],
        }
    }

    fn handle_scroll(&mut self, lines: i16, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        if self.view.is_server_list() {
            let new_offset = (self.scroll_offset as i16 + lines).max(0) as u16;
            self.scroll_offset = new_offset;
        }
        vec![]
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            match &self.view {
                McpView::ServerList => {
                    let relative_y = mouse.row.saturating_sub(area.y);
                    // border_top=1, header_lines=2 (count + blank), section_header=1
                    let header = 3u16;
                    if relative_y >= header && relative_y < area.height {
                        let clicked = (relative_y - header) as usize;
                        if clicked < self.servers.len() {
                            self.cursor = clicked;
                        }
                    }
                }
                McpView::ServerDetail { actions, .. } => {
                    // Detail: actions start after metadata lines
                    let inner_y = mouse.row.saturating_sub(area.y);
                    let meta_lines: u16 = 7; // Status + Auth + URL + Config + Capabilities + Tools + blank
                    if inner_y > meta_lines {
                        let clicked = (inner_y - meta_lines) as usize;
                        if clicked < actions.len() {
                            self.detail_cursor = clicked;
                        }
                    }
                }
            }
        }
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        self.total_content_lines().max(16)
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        if self.confirm_delete.is_some() {
            return vec![
                ("Enter".to_string(), lc.tr("key-delete").to_string()),
                ("Esc".to_string(), lc.tr("key-cancel").to_string()),
            ];
        }
        if self.view.is_server_list() {
            vec![
                (
                    "\u{2191}\u{2193}".to_string(),
                    lc.tr("key-move").to_string(),
                ),
                ("Enter".to_string(), lc.tr("key-detail").to_string()),
                ("Ctrl+R".to_string(), lc.tr("key-reconnect").to_string()),
                ("Ctrl+D".to_string(), lc.tr("key-delete").to_string()),
                ("Esc".to_string(), lc.tr("key-close").to_string()),
            ]
        } else {
            vec![
                (
                    "\u{2191}\u{2193}".to_string(),
                    lc.tr("key-move").to_string(),
                ),
                ("Enter".to_string(), lc.tr("key-execute").to_string()),
                ("Esc".to_string(), lc.tr("key-back").to_string()),
            ]
        }
    }
}

// ---------------------------------------------------------------------------
// Private render helpers
// ---------------------------------------------------------------------------

impl McpPanel {
    fn render_server_list(&mut self, f: &mut Frame, area: Rect, lc: &crate::i18n::LcRegistry) {
        let count = self.servers.len();

        let title = if count == 0 {
            lc.tr("mcp-panel-title-none")
        } else {
            lc.tr("mcp-panel-title")
        };

        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        // Server count
        lines.push(Line::from(vec![Span::styled(
            lc.tr_args(
                "mcp-server-count",
                &[("count".into(), (count as u64).into())],
            ),
            Style::default().fg(theme::MUTED),
        )]));
        lines.push(Line::from(""));

        if self.servers.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                lc.tr("mcp-no-servers"),
                Style::default().fg(theme::MUTED),
            )]));
        } else {
            // Partition by source
            let project: Vec<&McpServerEntry> = self
                .servers
                .iter()
                .filter(|s| s.source == "project")
                .collect();
            let global: Vec<&McpServerEntry> = self
                .servers
                .iter()
                .filter(|s| s.source != "project")
                .collect();

            if !project.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", lc.tr("mcp-section-project")),
                    Style::default().fg(theme::MUTED),
                )]));
                let project_start = 0usize;
                for (i, server) in project.iter().enumerate() {
                    let flat_idx = project_start + i;
                    self.render_server_entry(&mut lines, server, flat_idx);
                }
                lines.push(Line::from(""));
            }

            if !global.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", lc.tr("mcp-section-user")),
                    Style::default().fg(theme::MUTED),
                )]));
                let global_start = project.len();
                for (i, server) in global.iter().enumerate() {
                    let flat_idx = global_start + i;
                    self.render_server_entry(&mut lines, server, flat_idx);
                }
                lines.push(Line::from(""));
            }
        }

        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn render_server_entry(&self, lines: &mut Vec<Line>, server: &McpServerEntry, flat_idx: usize) {
        let is_cursor = flat_idx == self.cursor;
        let cursor_char = if is_cursor { "\u{276f} " } else { "  " };

        let (icon, icon_style, status_key) = status_render(&server.status);

        let name_style = if is_cursor {
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };

        lines.push(Line::from(vec![
            Span::styled(
                cursor_char.to_string(),
                Style::default().fg(theme::THINKING),
            ),
            Span::styled(server.name.clone(), name_style),
            Span::styled(" \u{00b7} ", Style::default().fg(theme::MUTED)),
            Span::styled(icon.to_string(), icon_style),
            Span::styled(format!(" {}", status_key), icon_style),
        ]));
    }

    fn render_server_detail(&mut self, f: &mut Frame, area: Rect, lc: &crate::i18n::LcRegistry) {
        let McpView::ServerDetail {
            server_name,
            actions,
            show_tools,
        } = &self.view
        else {
            return;
        };

        let inner = BorderedPanel::new(Span::styled(
            format!(" {} ", server_name),
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        // Find server entry
        let server = self.servers.iter().find(|s| &s.name == server_name);

        let label_width = 18;

        // Status line
        if let Some(info) = server {
            let (icon, style, _) = status_render(&info.status);
            lines.push(detail_line(
                label_width,
                &lc.tr("mcp-label-status"),
                &format!("{} {}", icon, info.status),
                style,
            ));
        }

        // Auth line
        if let Some(info) = server {
            let (auth_icon, auth_label, auth_style) = match info.auth_status.as_str() {
                "authorized" => ("\u{2714}", "authorized", Style::default().fg(theme::SAGE)),
                "needs_auth" => (
                    "\u{25b3}",
                    "needs_authorization",
                    Style::default().fg(theme::WARNING),
                ),
                _ => ("\u{2014}", "none", Style::default().fg(theme::MUTED)),
            };
            lines.push(detail_line(
                label_width,
                &lc.tr("mcp-label-auth"),
                &format!("{} {}", auth_icon, auth_label),
                auth_style,
            ));
        }

        // URL line
        if let Some(info) = server {
            if let Some(url) = &info.url {
                lines.push(detail_line(
                    label_width,
                    &lc.tr("mcp-label-url"),
                    url,
                    Style::default().fg(theme::TEXT),
                ));
            }
        }

        // Config location line
        if let Some(info) = server {
            lines.push(detail_line(
                label_width,
                &lc.tr("mcp-label-config-location"),
                &info.source,
                Style::default().fg(theme::TEXT),
            ));
        }

        // Capabilities line
        let mut capabilities = Vec::new();
        if let Some(info) = server {
            if info.tool_count > 0 {
                capabilities.push(lc.tr("mcp-capability-tools"));
            }
            if info.resource_count > 0 {
                capabilities.push(lc.tr("mcp-capability-resources"));
            }
        }
        lines.push(detail_line(
            label_width,
            &lc.tr("mcp-label-capabilities"),
            &capabilities.join(", "),
            Style::default().fg(theme::TEXT),
        ));

        // Tools line
        if let Some(info) = server {
            lines.push(detail_line(
                label_width,
                &lc.tr("mcp-label-tools"),
                &format!("{} tools", info.tool_count),
                Style::default().fg(theme::TEXT),
            ));
        }

        // Expanded tool list (placeholder -- tools data not in DTO yet)
        if *show_tools {
            lines.push(Line::from(vec![Span::styled(
                "      (tool list pending P3 Integration)",
                Style::default().fg(theme::MUTED),
            )]));
        }

        lines.push(Line::from(""));

        // Action menu
        for (i, action) in actions.iter().enumerate() {
            let is_cursor = i == self.detail_cursor;
            let cursor_char = if is_cursor { "\u{276f} " } else { "  " };
            let num = i + 1;
            let label = lc.tr(action.label_key());
            let style = if is_cursor {
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    cursor_char.to_string(),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(format!("{}. {}", num, label), style),
            ]));
        }

        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    // ---- Key action dispatchers ----

    fn do_enter(&mut self, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        match &self.view {
            McpView::ServerList => {
                if self.cursor >= self.servers.len() {
                    return vec![];
                }
                let server = &self.servers[self.cursor];
                let actions = Self::build_actions(server);
                self.view = McpView::ServerDetail {
                    server_name: server.name.clone(),
                    actions,
                    show_tools: false,
                };
                self.detail_cursor = 0;
                self.scroll_offset = 0;
                vec![]
            }
            McpView::ServerDetail { actions, .. } => {
                if self.detail_cursor >= actions.len() {
                    return vec![];
                }
                let action = actions[self.detail_cursor].clone();
                self.do_execute_action(&action)
            }
        }
    }

    fn do_back(&mut self) {
        if self.view.is_server_list() {
            return;
        }
        let name = match &self.view {
            McpView::ServerDetail { server_name, .. } => server_name.clone(),
            _ => return,
        };
        self.view = McpView::ServerList;
        // Restore cursor to the server we were viewing
        if let Some(pos) = self.servers.iter().position(|s| s.name == name) {
            self.cursor = pos;
        }
        self.detail_cursor = 0;
        self.scroll_offset = 0;
    }

    fn do_confirm_delete(&mut self, ctx: &PanelReadContext) -> Vec<PanelEffect> {
        let name = match self.confirm_delete.take() {
            Some(n) => n,
            None => return vec![],
        };

        // TODO(P3 Integration): SendToAcp with "delete_mcp_server" event.
        // For now, remove from local list and emit a notification.
        self.servers.retain(|s| s.name != name);
        self.cursor = self.cursor.min(self.servers.len().saturating_sub(1));

        let mut effects = vec![
            PanelEffect::SendToAcp {
                event: "delete_mcp_server".to_string(),
                data: serde_json::json!({ "name": name }),
            },
            PanelEffect::ShowNotification(
                ctx.lc
                    .tr_args("mcp-server-deleted", &[("name".into(), name.into())])
                    .to_string(),
            ),
        ];

        // If list is now empty, close the panel
        if self.servers.is_empty() {
            effects.push(PanelEffect::Close);
        }

        effects
    }

    fn do_execute_action(&mut self, action: &DetailAction) -> Vec<PanelEffect> {
        match action {
            DetailAction::ViewTools => {
                if let McpView::ServerDetail {
                    ref mut show_tools, ..
                } = self.view
                {
                    *show_tools = !*show_tools;
                }
                vec![]
            }
            DetailAction::ReAuthenticate => {
                let server_name = self.detail_server_name();
                self.do_back();
                // TODO(P3 Integration): SendToAcp with "mcp_reauthenticate" event.
                vec![PanelEffect::ShowNotification(format!(
                    "Re-authentication for '{}' not yet implemented",
                    server_name
                ))]
            }
            DetailAction::ClearAuth => {
                let server_name = self.detail_server_name();
                self.do_back();
                // TODO(P3 Integration): SendToAcp with "mcp_clear_auth" event.
                vec![PanelEffect::ShowNotification(format!(
                    "Clear auth for '{}' not yet implemented",
                    server_name
                ))]
            }
            DetailAction::Reconnect => {
                let server_name = self.detail_server_name();
                self.do_back();
                // TODO(P3 Integration): SendToAcp with "mcp_reconnect" event.
                vec![PanelEffect::ShowNotification(format!(
                    "Reconnect '{}' not yet implemented",
                    server_name
                ))]
            }
            DetailAction::Disable => {
                let server_name = self.detail_server_name();
                self.do_back();
                // TODO(P3 Integration): SendToAcp with "mcp_set_disabled" event.
                vec![PanelEffect::SendToAcp {
                    event: "mcp_set_disabled".to_string(),
                    data: serde_json::json!({ "name": server_name, "disabled": true }),
                }]
            }
            DetailAction::Enable => {
                let server_name = self.detail_server_name();
                self.do_back();
                // TODO(P3 Integration): SendToAcp with "mcp_set_disabled" event.
                vec![PanelEffect::SendToAcp {
                    event: "mcp_set_disabled".to_string(),
                    data: serde_json::json!({ "name": server_name, "disabled": false }),
                }]
            }
        }
    }

    fn detail_server_name(&self) -> String {
        match &self.view {
            McpView::ServerDetail { server_name, .. } => server_name.clone(),
            _ => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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

    fn ctrl_d_input() -> Input {
        Input {
            key: Key::Char('d'),
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// Construct a test `McpServerEntry`.
    fn make_server(name: &str, status: &str, source: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.to_string(),
            status: status.to_string(),
            auth_status: "none".to_string(),
            source: source.to_string(),
            transport_type: "stdio".to_string(),
            url: None,
            tool_count: 3,
            resource_count: 1,
        }
    }

    #[test]
    fn test_kind_returns_correct_variant() {
        let panel = McpPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Mcp);
    }

    #[test]
    fn test_esc_close_from_server_list() {
        let mut panel = McpPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_esc_from_detail_returns_to_list() {
        let servers = vec![
            make_server("s1", "connected", "project"),
            make_server("s2", "connected", "global"),
        ];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Enter to drill into detail
        panel.handle_key(enter_input(), &ctx);
        assert!(!panel.view.is_server_list());

        // Esc should go back to list, not close
        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(panel.view.is_server_list());
        assert_eq!(effects.len(), 0); // no Close
    }

    #[test]
    fn test_enter_navigates_to_detail() {
        let servers = vec![
            make_server("s1", "connected", "project"),
            make_server("s2", "disabled", "global"),
        ];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Enter on cursor=0 (s1)
        let effects = panel.handle_key(enter_input(), &ctx);
        assert_eq!(effects.len(), 0);
        match &panel.view {
            McpView::ServerDetail { server_name, .. } => {
                assert_eq!(server_name, "s1");
            }
            _ => panic!("expected ServerDetail"),
        }
    }

    #[test]
    fn test_render_does_not_panic_empty() {
        let mut panel = McpPanel::empty();
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_server_list() {
        let servers = vec![
            make_server("project-server", "connected", "project"),
            make_server("global-server", "disabled", "global"),
        ];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_server_detail() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Drill into detail
        panel.handle_key(enter_input(), &ctx);
        assert!(!panel.view.is_server_list());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_delete_flow() {
        let servers = vec![
            make_server("s1", "connected", "project"),
            make_server("s2", "connected", "global"),
        ];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Ctrl+D enters confirm_delete mode
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete.is_some());

        // Enter confirms delete
        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(panel.confirm_delete.is_none());
        assert_eq!(panel.servers.len(), 1); // s1 removed
        assert_eq!(panel.servers[0].name, "s2");

        // Should contain SendToAcp + ShowNotification (no Close since list not empty)
        assert!(effects.iter().any(|e| matches!(
            e,
            PanelEffect::SendToAcp {
                event,
                data,
            } if event == "delete_mcp_server" && data["name"] == "s1"
        )));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PanelEffect::ShowNotification(_))));
        assert!(!effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_delete_last_server_closes_panel() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Ctrl+D + Enter
        panel.handle_key(ctrl_d_input(), &ctx);
        let effects = panel.handle_key(enter_input(), &ctx);

        // Should include Close
        assert!(effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_delete_cancelled_by_non_enter_key() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Ctrl+D enters confirm mode
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete.is_some());

        // Any other key cancels (Esc)
        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(panel.confirm_delete.is_none());
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_navigation_in_list() {
        let servers = vec![
            make_server("s1", "connected", "project"),
            make_server("s2", "connected", "global"),
            make_server("s3", "disabled", "global"),
        ];
        let mut panel = McpPanel::new(servers);
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
    fn test_navigation_in_detail() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Drill into detail (should have 3 actions: ViewTools, Reconnect, Disable)
        panel.handle_key(enter_input(), &ctx);
        let action_count = match &panel.view {
            McpView::ServerDetail { actions, .. } => actions.len(),
            _ => panic!("expected ServerDetail"),
        };
        assert!(action_count >= 3);
        assert_eq!(panel.detail_cursor, 0);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.detail_cursor, 1);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.detail_cursor, 2);

        // Clamp at end
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.detail_cursor, 2);

        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.detail_cursor, 1);
    }

    #[test]
    fn test_sorting_project_first() {
        let servers = vec![
            make_server("alpha", "connected", "global"),
            make_server("beta", "connected", "project"),
            make_server("gamma", "connected", "global"),
        ];
        let panel = McpPanel::new(servers);
        // Project servers first, then alphabetical by name
        assert_eq!(panel.servers[0].name, "beta"); // project
        assert_eq!(panel.servers[1].name, "alpha"); // global
        assert_eq!(panel.servers[2].name, "gamma"); // global
    }

    #[test]
    fn test_status_bar_hints_list() {
        let panel = McpPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 5); // arrows, enter, ctrl+r, ctrl+d, esc
    }

    #[test]
    fn test_status_bar_hints_detail() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        panel.view = McpView::ServerDetail {
            server_name: "s1".to_string(),
            actions: vec![DetailAction::ViewTools],
            show_tools: false,
        };
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 3); // arrows, enter, esc
    }

    #[test]
    fn test_status_bar_hints_confirm_delete() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        panel.confirm_delete = Some("s1".to_string());
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2); // enter, esc
    }

    #[test]
    fn test_view_tools_toggle() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
        let ctx = make_ctx();

        // Drill into detail
        panel.handle_key(enter_input(), &ctx);
        let show_tools = match &panel.view {
            McpView::ServerDetail { show_tools, .. } => *show_tools,
            _ => panic!("expected ServerDetail"),
        };
        assert!(!show_tools);

        // Enter executes ViewTools action
        panel.handle_key(enter_input(), &ctx);
        let show_tools = match &panel.view {
            McpView::ServerDetail { show_tools, .. } => *show_tools,
            _ => panic!("expected ServerDetail"),
        };
        assert!(show_tools);

        // Toggle again
        panel.handle_key(enter_input(), &ctx);
        let show_tools = match &panel.view {
            McpView::ServerDetail { show_tools, .. } => *show_tools,
            _ => panic!("expected ServerDetail"),
        };
        assert!(!show_tools);
    }

    #[test]
    fn test_set_servers_replaces_data() {
        let mut panel = McpPanel::empty();
        assert_eq!(panel.servers.len(), 0);

        let servers = vec![
            make_server("s1", "connected", "project"),
            make_server("s2", "disabled", "global"),
        ];
        panel.set_servers(servers);
        assert_eq!(panel.servers.len(), 2);
        assert_eq!(panel.cursor(), 0);
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn test_handle_scroll() {
        let servers = vec![make_server("s1", "connected", "project")];
        let mut panel = McpPanel::new(servers);
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
}
