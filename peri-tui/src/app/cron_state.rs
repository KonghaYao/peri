use std::{any::Any, sync::Arc, time::Duration};

use parking_lot::Mutex;
use peri_middlewares::cron::{CronScheduler, CronTask};
use ratatui::{
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
    Frame,
};
use tui_textarea::Input;

use super::{
    panel_component::PanelComponent,
    panel_list::PanelList,
    panel_manager::{EventResult, PanelContext, PanelKind},
    App,
};

/// CronPanel 面板状态
#[derive(Debug, Clone)]
pub struct CronPanel {
    pub(crate) list: PanelList<CronTask>,
    /// 是否处于删除确认状态
    pub confirm_delete: bool,
}

impl CronPanel {
    pub fn new(tasks: Vec<CronTask>) -> Self {
        let mut list = PanelList::new();
        list.set_items(tasks);
        Self {
            list,
            confirm_delete: false,
        }
    }

    pub fn tasks(&self) -> &[CronTask] {
        self.list.items()
    }

    pub fn cursor(&self) -> usize {
        self.list.cursor()
    }

    pub fn scroll_offset(&self) -> u16 {
        self.list.scroll_offset()
    }

    pub fn refresh(&mut self, scheduler: &Mutex<CronScheduler>) {
        let new_tasks: Vec<CronTask> = scheduler.lock().list_tasks().into_iter().cloned().collect();
        self.list.set_items(new_tasks);
    }
}

impl PanelComponent for CronPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Cron
    }

    fn handle_key(&mut self, input: Input, ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;

        // confirm_delete mode
        if self.confirm_delete {
            match input {
                Input {
                    key: Key::Enter, ..
                } => {
                    self.do_confirm_delete(ctx);
                    // if tasks empty after delete, close
                    if self.list.is_empty() {
                        EventResult::ClosePanel
                    } else {
                        EventResult::Consumed
                    }
                }
                _ => {
                    self.confirm_delete = false;
                    EventResult::Consumed
                }
            }
        } else {
            match input {
                Input { key: Key::Up, .. } => {
                    self.list.move_cursor(-1);
                    EventResult::Consumed
                }
                Input { key: Key::Down, .. } => {
                    self.list.move_cursor(1);
                    EventResult::Consumed
                }
                Input {
                    key: Key::Enter, ..
                }
                | Input {
                    key: Key::Char(' '),
                    ..
                } => {
                    self.do_toggle(ctx);
                    EventResult::Consumed
                }
                Input { key: Key::Esc, .. } => EventResult::ClosePanel,
                Input {
                    key: Key::Char('d'),
                    ctrl: true,
                    ..
                } => {
                    if self.cursor() < self.tasks().len() {
                        self.confirm_delete = true;
                    }
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            }
        }
    }

    fn handle_scroll(&mut self, lines: i16, _ctx: &mut PanelContext<'_>) -> EventResult {
        self.list.handle_scroll(lines, 10);
        EventResult::Consumed
    }

    fn set_scroll_offset(&mut self, offset: u16) {
        self.list.set_scroll_offset(offset);
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &mut PanelContext<'_>,
    ) -> EventResult {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.list
                .handle_mouse_click(mouse.row, mouse.column, area, 2);
            EventResult::Consumed
        } else {
            EventResult::NotConsumed
        }
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        14
    }

    fn render(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        crate::ui::main_ui::panels::cron::render_cron_panel(f, self, app, area);
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self, _lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        if self.confirm_delete {
            return vec![
                ("Enter".to_string(), _lc.tr("hint-cron-confirm-delete")),
                ("Esc".to_string(), _lc.tr("key-cancel")),
            ];
        }
        vec![
            ("\u{2191}\u{2193}".to_string(), _lc.tr("key-move")),
            ("Enter/Space".to_string(), _lc.tr("key-switch")),
            ("Ctrl+D".to_string(), _lc.tr("key-delete")),
            ("Esc".to_string(), _lc.tr("key-close")),
        ]
    }
}

impl CronPanel {
    fn do_toggle(&mut self, ctx: &mut PanelContext<'_>) {
        let idx = self.cursor();
        if idx < self.tasks().len() {
            let id = self.tasks()[idx].id.clone();
            ctx.services.cron.scheduler.lock().toggle(&id);
            self.refresh(&ctx.services.cron.scheduler);
        }
    }

    fn do_confirm_delete(&mut self, ctx: &mut PanelContext<'_>) {
        self.confirm_delete = false;
        let idx = self.cursor();
        if idx < self.tasks().len() {
            let prompt_preview: String = self.tasks()[idx].prompt.chars().take(30).collect();
            let id = self.tasks()[idx].id.clone();
            ctx.services.cron.scheduler.lock().remove(&id);
            self.refresh(&ctx.services.cron.scheduler);
            ctx.session_mgr
                .current_mut()
                .messages
                .push_system_note(ctx.services.lc.tr_args(
                    "app-cron-deleted",
                    &[("preview".into(), prompt_preview.into())],
                ));
        }
    }
}

/// Cron 状态（App 子结构体）
pub struct CronState {
    pub scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronState {
    pub fn new() -> (Self, Arc<Mutex<CronScheduler>>) {
        let scheduler = CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0);
        let scheduler = Arc::new(Mutex::new(scheduler));

        let state = Self {
            scheduler: scheduler.clone(),
        };
        (state, scheduler)
    }

    /// Spawn CronManager tick task
    pub fn spawn_tick_task(scheduler: Arc<Mutex<CronScheduler>>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                scheduler.lock().tick();
            }
        });
    }
}

impl Default for CronState {
    fn default() -> Self {
        let (state, _scheduler) = Self::new();
        state
    }
}
