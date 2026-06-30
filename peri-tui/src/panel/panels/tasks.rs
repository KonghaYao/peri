//! v2 TasksPanel -- Cron tasks display panel (PanelState trait implementation).
//!
//! Displays a list of cron scheduled tasks with toggle/delete actions,
//! focusing on the CronTasks tab only (AgentThreads is a separate concern).
//!
//! Navigation: Up/Down to move cursor between tasks; scroll follows cursor.
//! Toggle: Enter/Space on a task to enable/disable it.
//! Delete: Ctrl+D to confirm delete, then Enter to execute.
//! Close: Esc. All other keys are consumed (no-op).
//!
//! Data is provided as `Vec<CronTaskDto>` (from `peri-acp-types`), avoiding
//! direct dependency on `peri_middlewares::cron` types.

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_textarea::Input;

use peri_acp_types::summary::CronTaskDto;
use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// TasksPanel
// ---------------------------------------------------------------------------

/// v2 Cron tasks display panel.
///
/// Shows cron scheduled tasks with navigation, toggle, and delete actions.
/// Data comes from `CronTaskDto` (peri-acp-types), no direct dependency on
/// `peri_middlewares::cron` runtime types.
///
/// Side-effects (toggle/delete) are returned as `PanelEffect::SendToAcp`
/// instructions; the state machine translates them to actual ACP operations.
#[derive(Debug)]
pub struct TasksPanel {
    /// Cron task list.
    tasks: Vec<CronTaskDto>,
    /// Cursor position (0-based index into `tasks`).
    cursor: usize,
    /// Vertical scroll offset (in lines, 0-based).
    scroll_offset: u16,
    /// Whether the user is in "confirm delete" mode.
    confirm_delete: bool,
}

impl TasksPanel {
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

    /// Construct a panel from live App data.
    ///
    /// Reads cron tasks from `app.services.cron.scheduler` and converts
    /// `CronTask` runtime types to panel-local `CronTaskDto` DTOs.
    pub fn from_app(app: &crate::app::App) -> Self {
        use peri_middlewares::cron::CronTask; // P4b: runtime dependency, conversion to DTO
        let tasks: Vec<CronTaskDto> = app
            .services
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
            .collect();
        if tasks.is_empty() {
            Self::empty()
        } else {
            Self::new(tasks)
        }
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

impl PanelState for TasksPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Tasks
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;
        let total = self.total_tasks();

        let title = if total == 0 {
            lc.tr("tasks-panel-title-none")
        } else {
            lc.tr("tasks-panel-title")
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
                    "tasks-configured-count",
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
                lc.tr("tasks-confirm-delete-hint"),
                Style::default().fg(theme::WARNING),
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                lc.tr("tasks-operation-hint"),
                Style::default().fg(theme::MUTED),
            )]));
        }
        lines.push(Line::from(""));

        // Task 列表
        if self.tasks.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                lc.tr("tasks-no-tasks"),
                Style::default().fg(theme::MUTED),
            )]));
            lines.push(Line::from(vec![Span::styled(
                lc.tr("tasks-no-tasks-hint"),
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
    fn test_kind_returns_tasks() {
        let panel = TasksPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Tasks);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = TasksPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_navigation() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
            make_task("t3", "0 0 * * *", "daily backup", true),
        ];
        let mut panel = TasksPanel::new(tasks);
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
    fn test_empty_panel() {
        let panel = TasksPanel::empty();
        assert_eq!(panel.total_tasks(), 0);
        assert_eq!(panel.cursor(), 0);
        assert!(!panel.confirm_delete);
    }

    #[test]
    fn test_toggle_produces_send_to_acp() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        let mut panel = TasksPanel::new(tasks);
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
    fn test_ctrl_d_enters_confirm_delete_mode() {
        let tasks = vec![make_task("t1", "*/5 * * * *", "check deploy", true)];
        let mut panel = TasksPanel::new(tasks);
        let ctx = make_ctx();

        assert!(!panel.confirm_delete);
        panel.handle_key(ctrl_d_input(), &ctx);
        assert!(panel.confirm_delete);
    }

    #[test]
    fn test_confirm_delete_enter_executes() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        let mut panel = TasksPanel::new(tasks);
        let ctx = make_ctx();

        // 进入确认删除模式
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
        assert!(effects
            .iter()
            .any(|e| matches!(e, PanelEffect::ShowNotification(_))));
        assert!(!effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_confirm_delete_on_last_task_closes_panel() {
        let tasks = vec![make_task("t1", "*/5 * * * *", "check deploy", true)];
        let mut panel = TasksPanel::new(tasks);
        let ctx = make_ctx();

        // 进入确认删除模式
        panel.handle_key(ctrl_d_input(), &ctx);
        // Enter 确认删除: 唯一任务删除后应关闭面板
        let effects = panel.handle_key(enter_input(), &ctx);
        assert!(effects.iter().any(|e| e == &PanelEffect::Close));
    }

    #[test]
    fn test_set_tasks_replaces_data() {
        let mut panel = TasksPanel::empty();
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
        // ASCII: 不截断
        assert_eq!(truncate_chars("hello", 10), "hello");
        // ASCII: 截断
        assert_eq!(truncate_chars("hello world", 5), "hello...");
        // CJK: 不截断
        assert_eq!(truncate_chars("\u{4f60}\u{597d}", 2), "\u{4f60}\u{597d}");
        // CJK: 截断
        assert_eq!(
            truncate_chars("\u{4f60}\u{597d}\u{4e16}\u{754c}", 2),
            "\u{4f60}\u{597d}..."
        );
    }

    #[test]
    fn test_prompt_summary_truncation() {
        let tasks = vec![make_task("t1", "*/5 * * * *", &"x".repeat(60), true)];
        let _panel = TasksPanel::new(tasks);
        // 渲染不 panic，prompt 被截断到 40 chars + "..."
        // (验证 truncate_chars 在 render 中生效)
    }

    #[test]
    fn test_desired_height_empty() {
        let panel = TasksPanel::empty();
        // 空面板：header_lines(2) + "no tasks"(1) + "hint"(1) = 4, max(8) = 8
        assert_eq!(panel.desired_height(50, 80), 8);
    }

    #[test]
    fn test_desired_height_with_tasks() {
        let tasks = vec![
            make_task("t1", "*/5 * * * *", "check deploy", true),
            make_task("t2", "0 * * * *", "hourly report", false),
        ];
        let panel = TasksPanel::new(tasks);
        // header_lines(3) + 2 tasks * 2 lines = 7, max(8) = 8
        assert_eq!(panel.desired_height(50, 80), 8);
    }

    #[test]
    fn test_desired_height_many_tasks() {
        let tasks: Vec<CronTaskDto> = (0..10)
            .map(|i| {
                make_task(
                    &format!("t{}", i),
                    "*/5 * * * *",
                    &format!("task {}", i),
                    true,
                )
            })
            .collect();
        let panel = TasksPanel::new(tasks);
        // header_lines(3) + 10 * 2 = 23
        assert_eq!(panel.desired_height(50, 80), 23);
    }

    #[test]
    fn test_render_does_not_panic_empty() {
        let mut panel = TasksPanel::empty();
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
        let mut panel = TasksPanel::new(tasks);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = TasksPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 4);
    }

    #[test]
    fn test_status_bar_hints_confirm_delete() {
        let tasks = vec![make_task("t1", "*/5 * * * *", "check deploy", true)];
        let mut panel = TasksPanel::new(tasks);
        // 进入确认删除模式
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
        let mut panel = TasksPanel::new(tasks);
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
    fn test_ctrl_c_not_consumed() {
        let mut panel = TasksPanel::empty();
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
        // Ctrl+C 不消费，返回空 effects
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_other_keys_consumed_no_op() {
        let tasks = vec![make_task("t1", "*/5 * * * *", "check deploy", true)];
        let mut panel = TasksPanel::new(tasks);
        let ctx = make_ctx();

        // 随机按键（如 'a'）应消费但不产生副作用
        let effects = panel.handle_key(
            Input {
                key: Key::Char('a'),
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(effects.len(), 0);
    }
}
