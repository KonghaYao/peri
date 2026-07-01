//! v2 CronPanel -- Cron schedule management panel (PanelState trait implementation).
//!
//! Displays cron scheduled tasks with toggle/delete actions,
//! using `CronTaskDto` from `peri-acp-types`.
//!
//! Navigation: Up/Down to move cursor between tasks; scroll follows cursor.
//! Toggle: Enter/Space on a task to enable/disable it.
//! Delete: Ctrl+D to confirm delete, then Enter to execute.
//! Close: Esc.  All other keys are consumed (no-op).
//!
//! Side-effects (toggle/delete) are returned as `PanelEffect::SendToAcp`
//! instructions; the state machine translates them to actual ACP operations.

use ratatui::Frame;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use tui_textarea::Input;

use peri_acp_types::summary::CronTaskDto;
use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::PanelState;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// CronPanel
// ---------------------------------------------------------------------------

/// v2 Cron schedule management panel.
///
/// Shows cron scheduled tasks with navigation, toggle, and delete actions.
/// Data comes from `CronTaskDto` (peri-acp-types), no direct dependency on
/// `peri_middlewares::cron` runtime types.
///
/// Side-effects (toggle/delete) are returned as `PanelEffect::SendToAcp`
/// instructions; the state machine translates them to actual ACP operations.
#[derive(Debug)]
pub struct CronPanel {
    /// Cron task list.
    tasks: Vec<CronTaskDto>,
    /// Cursor position (0-based index into `tasks`).
    cursor: usize,
    /// Vertical scroll offset (in lines, 0-based).
    scroll_offset: u16,
    /// Whether the user is in "confirm delete" mode.
    confirm_delete: bool,
}

impl CronPanel {
    /// Create an empty panel (no tasks loaded yet).
    ///
    /// Used by the registry factory. Tasks can be populated later via
    /// `set_tasks()` when ACP query results arrive.
    pub fn empty() -> Self {
        Self {
            tasks: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            confirm_delete: false,
        }
    }

    /// Construct a panel from the live `App` state, reading cron tasks.
    pub fn from_app(app: &crate::app::App) -> Self {
        let tasks = Self::tasks_from_app(app);
        if tasks.is_empty() {
            Self::empty()
        } else {
            Self::new(tasks)
        }
    }

    /// Pull fresh cron tasks from the live scheduler and convert to DTOs.
    ///
    /// Cron #30: extracted from `from_app` so `refresh` can reuse the same
    /// conversion without duplicating the CronTask → CronTaskDto mapping.
    fn tasks_from_app(app: &crate::app::App) -> Vec<CronTaskDto> {
        use peri_middlewares::cron::CronTask; // P4b: runtime dependency, conversion to DTO
        app.services
            .cron
            .scheduler
            .lock()
            .list_tasks()
            .into_iter()
            .map(|t: &CronTask| CronTaskDto {
                id: t.id.clone(),
                schedule: t.expression.clone(),
                prompt: t.prompt.clone(),
                next_fire: t.next_fire.map(|dt| dt.to_rfc3339()),
                enabled: t.enabled,
            })
            .collect()
    }

    /// Create a panel from a list of `CronTaskDto`.
    pub fn new(tasks: Vec<CronTaskDto>) -> Self {
        Self {
            tasks,
            cursor: 0,
            scroll_offset: 0,
            confirm_delete: false,
        }
    }

    /// Replace tasks data (e.g. after ACP query results arrive).
    pub fn set_tasks(&mut self, tasks: Vec<CronTaskDto>) {
        self.tasks = tasks;
        self.cursor = 0;
        self.scroll_offset = 0;
        self.confirm_delete = false;
    }

    /// Total number of cron tasks.
    pub fn total_tasks(&self) -> usize {
        self.tasks.len()
    }

    /// Current cursor position (0-based).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move cursor by `delta` (negative = up, positive = down).
    /// Clamps to valid range.
    fn move_cursor(&mut self, delta: i32) {
        let len = self.tasks.len();
        if len == 0 {
            return;
        }
        let new = (self.cursor as i32 + delta).clamp(0, (len - 1) as i32) as usize;
        self.cursor = new;
    }

    /// Ensure cursor is visible within `visible_lines` of the top.
    fn ensure_visible(&mut self, visible_lines: u16) {
        if self.tasks.is_empty() {
            return;
        }
        let line_per_entry: u16 = 2;
        let header_lines: u16 = if self.tasks.is_empty() { 2 } else { 3 };
        let cursor_line = header_lines + (self.cursor as u16) * line_per_entry;

        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        } else if cursor_line >= self.scroll_offset + visible_lines {
            self.scroll_offset = cursor_line - visible_lines + 1;
        }
    }

    /// Header lines count (stats + hint + blank).
    fn header_lines(&self) -> u16 {
        if self.tasks.is_empty() {
            2 // "no tasks" + blank
        } else {
            3 // stats + hint + blank
        }
    }

    /// Total content lines.
    fn total_content_lines(&self) -> u16 {
        let mut h = self.header_lines();
        for _ in &self.tasks {
            h += 2; // 每条 task: 标签行 + 详情行
        }
        h
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

impl PanelState for CronPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Cron
    }

    /// Cron #30: refresh tasks from live scheduler, preserving cursor +
    /// scroll + confirm_delete state.
    ///
    /// Bug: prior to this hook, CronPanel cached `tasks` at `from_app`
    /// time. The user's own toggle (Enter/Space) and delete (Ctrl+D) emit
    /// SendToAcp but don't mutate `self.tasks` locally, so the panel kept
    /// showing pre-action state. Newly created crons (via agent
    /// conversation) also never appeared. Furthermore, `set_tasks` resets
    /// cursor/scroll — using it in refresh would cause "jump to top"
    /// every render.
    ///
    /// Fix: pull fresh tasks via `tasks_from_app`, replace `self.tasks`
    /// in-place. Manually clamp cursor to new bounds (preserves position
    /// when possible). Don't touch scroll_offset or confirm_delete —
    /// those represent user intent mid-action.
    fn refresh(&mut self, app: &crate::app::App) {
        let fresh_tasks = Self::tasks_from_app(app);
        self.tasks = fresh_tasks;
        if self.cursor >= self.tasks.len() && !self.tasks.is_empty() {
            self.cursor = self.tasks.len().saturating_sub(1);
        } else if self.tasks.is_empty() {
            self.cursor = 0;
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;
        let total = self.total_tasks();

        let title = if total == 0 {
            lc.tr("cron-panel-title-none")
        } else {
            lc.tr("cron-panel-title")
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
            let enabled_count = self.tasks.iter().filter(|t| t.enabled).count();
            lines.push(Line::from(vec![Span::styled(
                lc.tr_args(
                    "cron-configured-count",
                    &[
                        ("count".into(), total.to_string().into()),
                        ("enabled".into(), enabled_count.to_string().into()),
                    ],
                ),
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD),
            )]));
        }

        // 操作提示
        if self.confirm_delete {
            lines.push(Line::from(vec![Span::styled(
                lc.tr("cron-confirm-delete-hint"),
                Style::default().fg(theme::WARNING),
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                lc.tr("cron-operation-hint"),
                Style::default().fg(theme::MUTED),
            )]));
        }
        lines.push(Line::from(""));

        // Task 列表
        if self.tasks.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                lc.tr("cron-no-tasks"),
                Style::default().fg(theme::MUTED),
            )]));
            lines.push(Line::from(vec![Span::styled(
                lc.tr("cron-no-tasks-hint"),
                Style::default().fg(theme::MUTED),
            )]));
        } else {
            for (i, task) in self.tasks.iter().enumerate() {
                let is_cursor = self.cursor == i;
                let cursor_char = if is_cursor { "\u{276f}" } else { " " };

                let name_style = if is_cursor {
                    Style::default()
                        .fg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                let enabled_label = if task.enabled { "ON" } else { "OFF" };
                let enabled_style = if task.enabled {
                    Style::default().fg(theme::SAGE)
                } else {
                    Style::default().fg(theme::MUTED)
                };

                // 标签行: cursor + index + schedule + enabled
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} {}. ", cursor_char, i + 1), name_style),
                    Span::styled(format!("{} ", task.schedule), name_style),
                    Span::styled(format!("[{}] ", enabled_label), enabled_style),
                ]));

                // 详情行（缩进显示 prompt 摘要 + next_fire）
                let prompt_summary = truncate_chars(&task.prompt, 40);
                let next_fire_display = task.next_fire.as_deref().unwrap_or("--").to_string();

                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(prompt_summary, Style::default().fg(theme::TEXT)),
                    Span::raw(" "),
                    Span::styled(next_fire_display, Style::default().fg(theme::MUTED)),
                ]));
            }
        }

        // 截断到可视区域
        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn handle_key(&mut self, input: Input, ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;

        // confirm_delete mode
        if self.confirm_delete {
            match input {
                // Enter: 执行删除
                Input {
                    key: Key::Enter, ..
                } => {
                    self.confirm_delete = false;
                    if let Some(task) = self.tasks.get(self.cursor) {
                        let id = task.id.clone();
                        let prompt_preview: String = task.prompt.chars().take(30).collect();
                        let mut effects = vec![
                            PanelEffect::SendToAcp {
                                event: "delete_cron_task".to_string(),
                                data: serde_json::json!({ "id": id }),
                            },
                            PanelEffect::ShowNotification(ctx.lc.tr_args(
                                "app-cron-deleted",
                                &[("preview".into(), prompt_preview.into())],
                            )),
                        ];
                        // 删除后若列表将空，关闭面板
                        if self.tasks.len() <= 1 {
                            effects.push(PanelEffect::Close);
                        }
                        return effects;
                    }
                    vec![]
                }
                // Esc: 取消确认
                Input { key: Key::Esc, .. } => {
                    self.confirm_delete = false;
                    vec![]
                }
                // 其他按键: 取消确认并消费
                _ => {
                    self.confirm_delete = false;
                    vec![]
                }
            }
        } else {
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
                // Enter / Space: 切换 cron 任务 enabled 状态
                Input {
                    key: Key::Enter, ..
                }
                | Input {
                    key: Key::Char(' '),
                    ..
                } => {
                    if let Some(task) = self.tasks.get(self.cursor) {
                        let id = task.id.clone();
                        return vec![PanelEffect::SendToAcp {
                            event: "toggle_cron_task".to_string(),
                            data: serde_json::json!({ "id": id }),
                        }];
                    }
                    vec![]
                }
                // Ctrl+D: 进入确认删除模式
                Input {
                    key: Key::Char('d'),
                    ctrl: true,
                    ..
                } => {
                    if !self.tasks.is_empty() {
                        self.confirm_delete = true;
                    }
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
            if relative_y >= 2 {
                let header = self.header_lines();
                let clicked_line = relative_y.saturating_sub(header);
                let clicked_entry = (clicked_line / 2) as usize;
                if clicked_entry < self.tasks.len() {
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
        if self.confirm_delete {
            return vec![
                ("Enter".to_string(), lc.tr("hint-cron-confirm-delete")),
                ("Esc".to_string(), lc.tr("key-cancel")),
            ];
        }
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                lc.tr("key-move").to_string(),
            ),
            ("Enter/Space".to_string(), lc.tr("key-switch").to_string()),
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

    fn enter_input() -> Input {
        Input {
            key: Key::Enter,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn space_input() -> Input {
        Input {
            key: Key::Char(' '),
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

    /// 构造测试用 CronTaskDto。
    fn make_task(id: &str, schedule: &str, prompt: &str, enabled: bool) -> CronTaskDto {
        CronTaskDto {
            id: id.to_string(),
            schedule: schedule.to_string(),
            prompt: prompt.to_string(),
            next_fire: None,
            enabled,
        }
    }

    #[test]
    fn test_kind_returns_correct_variant() {
        let panel = CronPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Cron);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = CronPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_arrow_keys_move_cursor() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
            make_task("t3", "0 0 * * *", "daily backup", true),
        ];
        let mut panel = CronPanel::new(tasks);
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
    fn test_delete_flow_with_confirmation() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        let mut panel = CronPanel::new(tasks);
        let ctx = make_ctx();

        // Ctrl+D 进入确认删除模式
        assert!(!panel.confirm_delete);
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete);

        // Esc 取消确认
        let effects = panel.handle_key(esc_input(), &ctx);
        assert!(!panel.confirm_delete);
        assert_eq!(effects.len(), 0);

        // 再次进入确认删除模式
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete);

        // Enter 确认删除: 应产生 SendToAcp + ShowNotification（列表不会空，无 Close）
        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(!panel.confirm_delete);
        assert!(effects.len() >= 2);
        assert!(effects.iter().any(|e| matches!(
            e,
            PanelEffect::SendToAcp {
                event,
                data,
            } if event == "delete_cron_task" && data["id"] == "t1"
        )));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, PanelEffect::ShowNotification(_)))
        );
        assert!(!effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_delete_last_task_closes_panel() {
        let tasks = vec![make_task("t1", "*/5 * * * *", "check deploy", true)];
        let mut panel = CronPanel::new(tasks);
        let ctx = make_ctx();

        // 进入确认删除模式
        panel.handle_key(ctrl_d_input(), &ctx);
        // Enter 确认删除: 唯一任务删除后应关闭面板
        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_render_does_not_panic_empty() {
        let mut panel = CronPanel::empty();
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_with_tasks() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy status", true),
            make_task("t2", "0 * * * *", "hourly report generation", false),
        ];
        let mut panel = CronPanel::new(tasks);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_toggle_produces_send_to_acp() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        let mut panel = CronPanel::new(tasks);
        let ctx = make_ctx();

        // Enter on cursor=0 (t1): should produce SendToAcp with toggle
        let effects = panel.handle_key(enter_input(), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SendToAcp { event, data } => {
                assert_eq!(event, "toggle_cron_task");
                assert_eq!(data["id"], "t1");
            }
            _ => panic!("expected SendToAcp, got {:?}", effects[0]),
        }

        // Move to cursor=1 (t2), Space should also toggle
        panel.handle_key(down_input(), &ctx);
        let effects = panel.handle_key(space_input(), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SendToAcp { event, data } => {
                assert_eq!(event, "toggle_cron_task");
                assert_eq!(data["id"], "t2");
            }
            _ => panic!("expected SendToAcp, got {:?}", effects[0]),
        }
    }

    #[test]
    fn test_status_bar_hints_normal() {
        let panel = CronPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 4);
    }

    #[test]
    fn test_status_bar_hints_confirm_delete() {
        let tasks = vec![make_task("t1", "*/5 * * * *", "check deploy", true)];
        let mut panel = CronPanel::new(tasks);
        panel.confirm_delete = true;
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn test_handle_scroll() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        let mut panel = CronPanel::new(tasks);
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
    fn test_ctrl_c_not_consumed() {
        let mut panel = CronPanel::empty();
        let ctx = make_ctx();
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
    fn test_set_tasks_replaces_data() {
        let mut panel = CronPanel::empty();
        assert_eq!(panel.total_tasks(), 0);

        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        panel.set_tasks(tasks);
        assert_eq!(panel.total_tasks(), 2);
        assert_eq!(panel.cursor(), 0);
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn test_truncate_chars_cjk_safe() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello world", 5), "hello...");
        assert_eq!(truncate_chars("\u{4f60}\u{597d}", 2), "\u{4f60}\u{597d}");
        assert_eq!(
            truncate_chars("\u{4f60}\u{597d}\u{4e16}\u{754c}", 2),
            "\u{4f60}\u{597d}..."
        );
    }

    // -----------------------------------------------------------------------
    // Cron #30: refresh hook regression tests
    // -----------------------------------------------------------------------
    //
    // Bug being prevented: prior to Cron #30, CronPanel cached `tasks` at
    // `from_app` time. New tasks registered later (via agent conversation
    // or /cron command) didn't appear until the user closed and reopened
    // the panel. `refresh` is called by `draw_now` before every render.
    //
    // These tests use a live App + scheduler to verify the panel picks up
    // mutations made AFTER `from_app`.

    /// Helper: build a headless App with no cron tasks.
    async fn make_empty_app() -> crate::app::App {
        let (app, _handle) = crate::app::App::new_headless(80, 24).await;
        app
    }

    /// Cron #30: refresh must pull tasks registered AFTER `from_app` time.
    ///
    /// Simulates: user opens CronPanel (panel is empty), agent registers a
    /// cron task in the background, next render cycle calls refresh — the
    /// panel must now show the new task without requiring Esc+reopen.
    #[tokio::test]
    async fn test_refresh_pulls_tasks_registered_after_open() {
        let app = make_empty_app().await;
        // sanity: app starts with no cron tasks
        assert!(app.services.cron.scheduler.lock().list_tasks().is_empty());

        // user opens panel before any tasks exist
        let mut panel = CronPanel::from_app(&app);
        assert_eq!(panel.total_tasks(), 0);

        // agent registers a task while panel is open
        let _id1 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("*/5 * * * *", "check deploy")
            .expect("register should succeed");

        // refresh must pick it up
        panel.refresh(&app);
        assert_eq!(panel.total_tasks(), 1);
        assert_eq!(panel.tasks[0].schedule, "*/5 * * * *");
        assert_eq!(panel.tasks[0].prompt, "check deploy");
        assert!(panel.tasks[0].enabled);
    }

    /// Cron #30: refresh must preserve cursor when it's still in bounds.
    ///
    /// Simulates: user opens CronPanel with 2 tasks, moves cursor to task
    /// #2, agent registers a 3rd task — refresh must keep cursor at #2
    /// (the user's selection), not reset to 0.
    #[tokio::test]
    async fn test_refresh_preserves_cursor_when_in_bounds() {
        let app = make_empty_app().await;
        let _id1 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("*/5 * * * *", "task one")
            .unwrap();
        let _id2 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("0 * * * *", "task two")
            .unwrap();

        let mut panel = CronPanel::from_app(&app);
        assert_eq!(panel.total_tasks(), 2);

        // user moves cursor to task #2 (index 1)
        panel.cursor = 1;
        let cursor_before = panel.cursor;

        // refresh with same data — cursor must be preserved
        panel.refresh(&app);
        assert_eq!(panel.cursor, cursor_before, "cursor must be preserved");
        assert_eq!(panel.total_tasks(), 2);
    }

    /// Cron #30: refresh must clamp cursor when tasks shrink below cursor.
    ///
    /// Simulates: user opens CronPanel with 3 tasks, cursor on task #3,
    /// agent removes task #3 via /cron delete — refresh must clamp cursor
    /// to the last available task (not panic, not stay at invalid index).
    #[tokio::test]
    async fn test_refresh_clamps_cursor_when_tasks_shrink() {
        let app = make_empty_app().await;
        let id1 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("*/5 * * * *", "task one")
            .unwrap();
        let _id2 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("0 * * * *", "task two")
            .unwrap();
        let id3 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("0 0 * * *", "task three")
            .unwrap();

        let mut panel = CronPanel::from_app(&app);
        assert_eq!(panel.total_tasks(), 3);

        // user moves cursor to task #3 (index 2)
        panel.cursor = 2;

        // task #3 is removed externally (simulating /cron delete or agent action)
        let removed = app.services.cron.scheduler.lock().remove(&id3);
        assert!(removed);
        // also remove id1 to verify the clamp uses NEW last index, not the old one
        app.services.cron.scheduler.lock().remove(&id1);
        // remaining tasks: [id2] (1 task, valid index = 0)

        // refresh must clamp cursor to 0 (only 1 task left)
        panel.refresh(&app);
        assert_eq!(panel.total_tasks(), 1);
        assert_eq!(
            panel.cursor, 0,
            "cursor must be clamped to last valid index after task removal"
        );
    }

    /// Cron #30: refresh must NOT reset scroll_offset or confirm_delete.
    ///
    /// Simulates: user is mid-delete-confirmation (Ctrl+D pressed), agent
    /// state updates trigger a refresh — refresh must preserve the
    /// confirm_delete flag so the user's intent isn't lost.
    #[tokio::test]
    async fn test_refresh_preserves_confirm_delete_state() {
        let app = make_empty_app().await;
        let _id1 = app
            .services
            .cron
            .scheduler
            .lock()
            .register("*/5 * * * *", "task one")
            .unwrap();

        let mut panel = CronPanel::from_app(&app);

        // user pressed Ctrl+D, entered confirm-delete mode
        panel.confirm_delete = true;
        panel.scroll_offset = 5;

        // refresh with same data
        panel.refresh(&app);

        // confirm_delete + scroll_offset must be preserved (user intent)
        assert!(
            panel.confirm_delete,
            "confirm_delete must be preserved across refresh (user mid-action)"
        );
        assert_eq!(
            panel.scroll_offset, 5,
            "scroll_offset must be preserved across refresh"
        );
    }
}
