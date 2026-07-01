//! v2 HooksPanel -- Hooks display panel (PanelState trait implementation).
//!
//! Displays registered hooks grouped by event name. The panel is read-only:
//! hooks are configured via plugin `hooks/hooks.json` files.
//!
//! Navigation: Up/Down to move cursor between hooks; scroll follows cursor.
//! Close: Esc. All other keys are consumed (no-op).
//!
//! Data is provided as `Vec<HookDto>` (from `peri-acp-types`), avoiding
//! direct dependency on `peri_middlewares::hooks` types.

use ratatui::Frame;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use tui_textarea::Input;

use peri_acp_types::summary::HookDto;
use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::PanelState;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// HookEntry -- display-friendly grouping
// ---------------------------------------------------------------------------

/// A hook entry displayed in the panel list.
///
/// Derived from `HookDto` plus computed display fields.
#[derive(Debug, Clone)]
pub struct HookEntry {
    /// Original DTO data.
    pub dto: HookDto,
    /// Truncated command/prompt summary (max 40 chars, CJK-safe).
    pub command_summary: String,
}

impl HookEntry {
    fn from_dto(dto: HookDto) -> Self {
        let command_summary = truncate_chars(&dto.command, 40);
        Self {
            dto,
            command_summary,
        }
    }
}

/// 字符级截断（CJK 安全，避免 `&s[..N]` 对多字节字符 panic）。
fn truncate_chars(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}...", truncated)
    }
}

// ---------------------------------------------------------------------------
// HooksPanel
// ---------------------------------------------------------------------------

/// v2 Hooks display panel.
///
/// Read-only: shows hooks grouped by event name with navigation.
/// Data comes from `HookDto` (peri-acp-types), no direct dependency on
/// `peri_middlewares::hooks` runtime types.
#[derive(Debug)]
pub struct HooksPanel {
    /// Flat list of hook entries (ordered by event name, then by id).
    entries: Vec<HookEntry>,
    /// Cursor position (0-based index into `entries`).
    cursor: usize,
    /// Vertical scroll offset (in lines, 0-based).
    scroll_offset: u16,
}

impl HooksPanel {
    /// Create an empty panel (no hooks loaded yet).
    ///
    /// Used by the registry factory. Hooks can be populated later via
    /// `set_hooks()` when ACP query results arrive.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
        }
    }

    /// Construct a panel from live App data.
    ///
    /// Hook data arrives asynchronously via ACP queries. Currently
    /// delegates to `empty()` with data populated later via
    /// `set_hooks()` when ACP results arrive.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }

    /// Create a panel from a list of `HookDto`.
    ///
    /// Entries are sorted by event name for consistent display.
    pub fn new(hooks: Vec<HookDto>) -> Self {
        let mut entries: Vec<HookEntry> = hooks.into_iter().map(HookEntry::from_dto).collect();
        // 按 event name 排序，同 event 内按 id 排序
        entries.sort_by(|a, b| (&a.dto.event, &a.dto.id).cmp(&(&b.dto.event, &b.dto.id)));
        Self {
            entries,
            cursor: 0,
            scroll_offset: 0,
        }
    }

    /// Replace hooks data (e.g. after ACP query results arrive).
    pub fn set_hooks(&mut self, hooks: Vec<HookDto>) {
        let mut entries: Vec<HookEntry> = hooks.into_iter().map(HookEntry::from_dto).collect();
        entries.sort_by(|a, b| (&a.dto.event, &a.dto.id).cmp(&(&b.dto.event, &b.dto.id)));
        self.entries = entries;
        self.cursor = 0;
        self.scroll_offset = 0;
    }

    /// Total number of hook entries.
    pub fn total_hooks(&self) -> usize {
        self.entries.len()
    }

    /// Current cursor position (0-based).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move cursor by `delta` (negative = up, positive = down).
    /// Clamps to valid range.
    fn move_cursor(&mut self, delta: i32) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let new = (self.cursor as i32 + delta).clamp(0, (len - 1) as i32) as usize;
        self.cursor = new;
    }

    /// Ensure cursor is visible within `visible_lines` of the top.
    fn ensure_visible(&mut self, visible_lines: u16) {
        if self.entries.is_empty() {
            return;
        }
        // 每个条目占 2 行（标签行 + 详情行），加上 header_lines
        let line_per_entry: u16 = 2;
        let header_lines: u16 = if self.entries.is_empty() { 2 } else { 3 };
        let cursor_line = header_lines + (self.cursor as u16) * line_per_entry;

        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        } else if cursor_line >= self.scroll_offset + visible_lines {
            self.scroll_offset = cursor_line - visible_lines + 1;
        }
    }

    /// Header lines count (stats + hint + blank).
    fn header_lines(&self) -> u16 {
        if self.entries.is_empty() {
            2 // "no hooks" + blank
        } else {
            3 // stats + hint + blank
        }
    }

    /// Total content lines.
    fn total_content_lines(&self) -> u16 {
        let mut h = self.header_lines();
        for _ in &self.entries {
            h += 2; // 每条 hook: 标签行 + 详情行
        }
        h
    }

    /// Build event description string from event name.
    fn event_description(event: &str) -> &'static str {
        match event {
            "PreToolUse" => "Before tool execution",
            "PostToolUse" => "After tool execution",
            "PostToolUseFailure" => "After tool execution fails",
            "PermissionRequest" => "Before auto mode classifier decides",
            "UserPromptSubmit" => "When user submits a prompt",
            "SessionStart" => "When a new session starts",
            "SessionEnd" => "When a session ends",
            "Stop" => "When agent stops",
            "StopFailure" => "When agent stops with failure",
            "PostToolBatch" => "When all parallel tools complete",
            "SubagentStart" => "When a subagent starts",
            "SubagentStop" => "When a subagent stops",
            "PreCompact" => "Before context compaction",
            "PostCompact" => "After context compaction",
            "Notification" => "When agent needs user input",
            _ => "",
        }
    }
}

impl PanelState for HooksPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Hooks
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;
        let total = self.total_hooks();

        let title = if total == 0 {
            lc.tr("hooks-panel-title-none")
        } else {
            lc.tr("hooks-panel-title")
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

        // 统计行
        if total > 0 {
            lines.push(Line::from(vec![Span::styled(
                lc.tr_args(
                    "hooks-configured-count",
                    &[("count".into(), total.to_string().into())],
                ),
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD),
            )]));
        }

        // 只读提示
        lines.push(Line::from(vec![Span::styled(
            lc.tr("hooks-readonly-hint"),
            Style::default().fg(theme::MUTED),
        )]));
        lines.push(Line::from(""));

        // Hook 列表
        if self.entries.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                lc.tr("hooks-no-hooks"),
                Style::default().fg(theme::MUTED),
            )]));
            lines.push(Line::from(vec![Span::styled(
                lc.tr("hooks-no-hooks-hint"),
                Style::default().fg(theme::MUTED),
            )]));
        } else {
            for (i, entry) in self.entries.iter().enumerate() {
                let is_cursor = self.cursor == i;
                let cursor_char = if is_cursor { "\u{276f}" } else { " " };

                let name_style = if is_cursor {
                    Style::default()
                        .fg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let enabled_label = if entry.dto.enabled { "ON" } else { "OFF" };
                let enabled_style = if entry.dto.enabled {
                    Style::default().fg(theme::SAGE)
                } else {
                    Style::default().fg(theme::MUTED)
                };

                let description = Self::event_description(&entry.dto.event);

                // 标签行: cursor + index + event + enabled + description
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} {}. ", cursor_char, i + 1), name_style),
                    Span::styled(format!("{} ", entry.dto.event), name_style),
                    Span::styled(format!("[{}] ", enabled_label), enabled_style),
                    Span::styled(description, Style::default().fg(theme::MUTED)),
                ]));

                // 详情行（缩进显示 command 摘要）
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        entry.command_summary.as_str(),
                        Style::default().fg(theme::TEXT),
                    ),
                ]));
            }
        }

        // 截断到可视区域
        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
            // Esc: 关闭面板
            Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
            // Up: 光标上移
            Input { key: Key::Up, .. } => {
                self.move_cursor(-1);
                self.ensure_visible(10);
                vec![]
            }
            // Down: 光标下移
            Input { key: Key::Down, .. } => {
                self.move_cursor(1);
                self.ensure_visible(10);
                vec![]
            }
            // Ctrl+C: 不消费，让上层处理
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
            // border_top=1
            if relative_y >= 2 {
                // header_lines=3 (stats + hint + blank), 每条 2 行
                let header = self.header_lines();
                let clicked_line = relative_y.saturating_sub(header);
                let clicked_entry = (clicked_line / 2) as usize;
                if clicked_entry < self.entries.len() {
                    self.cursor = clicked_entry;
                }
            }
        }
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        self.total_content_lines().max(8)
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                lc.tr("key-move").to_string(),
            ),
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

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui_textarea::Key;

    use super::*;
    use crate::panel::PanelState;
    use crate::panel::read_context::{PanelReadContext, ServiceRegistrySnapshot};

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
                            services: snapshot.clone(),
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

    /// 构造测试用 HookDto。
    fn make_hook(id: &str, event: &str, command: &str, enabled: bool) -> HookDto {
        HookDto {
            id: id.to_string(),
            event: event.to_string(),
            command: command.to_string(),
            enabled,
        }
    }

    #[test]
    fn test_kind_returns_hooks() {
        let panel = HooksPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Hooks);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = HooksPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_navigation() {
        let hooks = vec![
            make_hook("h1", "PreToolUse", "echo pre-tool", true),
            make_hook("h2", "PostToolUse", "echo post-tool", true),
            make_hook("h3", "SessionStart", "echo session-start", false),
        ];
        let mut panel = HooksPanel::new(hooks);
        let ctx = make_ctx();

        // 初始 cursor=0
        assert_eq!(panel.cursor(), 0);

        // Down -> cursor=1
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 1);

        // Down -> cursor=2
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 2);

        // Down -> clamped at 2
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.cursor(), 2);

        // Up -> cursor=1
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 1);

        // Up -> cursor=0
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 0);

        // Up -> clamped at 0
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.cursor(), 0);
    }

    #[test]
    fn test_new_sorts_by_event() {
        let hooks = vec![
            make_hook("c", "SessionStart", "echo c", true),
            make_hook("a", "PreToolUse", "echo a", true),
            make_hook("b", "PostToolUse", "echo b", true),
        ];
        let panel = HooksPanel::new(hooks);
        // 按 event name 排序: PostToolUse < PreToolUse < SessionStart
        assert_eq!(panel.entries[0].dto.event, "PostToolUse");
        assert_eq!(panel.entries[1].dto.event, "PreToolUse");
        assert_eq!(panel.entries[2].dto.event, "SessionStart");
    }

    #[test]
    fn test_empty_panel() {
        let panel = HooksPanel::empty();
        assert_eq!(panel.total_hooks(), 0);
        assert_eq!(panel.cursor(), 0);
    }

    #[test]
    fn test_set_hooks_replaces_data() {
        let mut panel = HooksPanel::empty();
        assert_eq!(panel.total_hooks(), 0);

        let hooks = vec![
            make_hook("h1", "PreToolUse", "echo pre", true),
            make_hook("h2", "PostToolUse", "echo post", false),
        ];
        panel.set_hooks(hooks);
        assert_eq!(panel.total_hooks(), 2);
        assert_eq!(panel.cursor(), 0);
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn test_truncate_chars_cjk_safe() {
        // ASCII: 不截断
        assert_eq!(truncate_chars("hello", 10), "hello");
        // ASCII: 截断
        assert_eq!(truncate_chars("hello world", 5), "hello...");
        // CJK: 不截断（每个字符 1 个 Unicode scalar）
        assert_eq!(truncate_chars("\u{4f60}\u{597d}", 2), "\u{4f60}\u{597d}");
        // CJK: 截断
        assert_eq!(
            truncate_chars("\u{4f60}\u{597d}\u{4e16}\u{754c}", 2),
            "\u{4f60}\u{597d}..."
        );
    }

    #[test]
    fn test_command_summary_truncation() {
        let hooks = vec![make_hook("h1", "PreToolUse", &"x".repeat(60), true)];
        let panel = HooksPanel::new(hooks);
        // 40 chars + "..."
        assert_eq!(panel.entries[0].command_summary.len(), 40 + 3);
    }

    #[test]
    fn test_desired_height_empty() {
        let panel = HooksPanel::empty();
        // 空面板：header_lines(2) + "no hooks"(1) + "hint"(1) = 4, max(8) = 8
        assert_eq!(panel.desired_height(50, 80), 8);
    }

    #[test]
    fn test_desired_height_with_hooks() {
        let hooks = vec![
            make_hook("h1", "PreToolUse", "echo a", true),
            make_hook("h2", "PostToolUse", "echo b", true),
        ];
        let panel = HooksPanel::new(hooks);
        // header_lines(3) + 2 entries * 2 lines = 7, max(8) = 8
        assert_eq!(panel.desired_height(50, 80), 8);
    }

    #[test]
    fn test_desired_height_many_hooks() {
        let hooks: Vec<HookDto> = (0..10)
            .map(|i| {
                make_hook(
                    &format!("h{}", i),
                    "PreToolUse",
                    &format!("echo {}", i),
                    true,
                )
            })
            .collect();
        let panel = HooksPanel::new(hooks);
        // header_lines(3) + 10 * 2 = 23
        assert_eq!(panel.desired_height(50, 80), 23);
    }

    #[test]
    fn test_render_does_not_panic_empty() {
        let mut panel = HooksPanel::empty();
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_with_hooks() {
        let hooks = vec![
            make_hook("h1", "PreToolUse", "echo pre-tool hook", true),
            make_hook("h2", "SessionStart", "echo session start", false),
        ];
        let mut panel = HooksPanel::new(hooks);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = HooksPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn test_handle_scroll() {
        let hooks = vec![
            make_hook("h1", "PreToolUse", "echo a", true),
            make_hook("h2", "PostToolUse", "echo b", true),
        ];
        let mut panel = HooksPanel::new(hooks);
        let ctx = make_ctx();

        // scroll down 1 line
        panel.handle_scroll(1, &ctx);
        assert_eq!(panel.scroll_offset, 1);

        // scroll down 5 more lines
        panel.handle_scroll(5, &ctx);
        assert_eq!(panel.scroll_offset, 6);

        // scroll up 3 lines
        panel.handle_scroll(-3, &ctx);
        assert_eq!(panel.scroll_offset, 3);

        // scroll up beyond 0 -> clamped to 0
        panel.handle_scroll(-10, &ctx);
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn test_event_description() {
        assert_eq!(
            HooksPanel::event_description("PreToolUse"),
            "Before tool execution"
        );
        assert_eq!(
            HooksPanel::event_description("SessionStart"),
            "When a new session starts"
        );
        assert_eq!(HooksPanel::event_description("UnknownEvent"), "");
    }
}
