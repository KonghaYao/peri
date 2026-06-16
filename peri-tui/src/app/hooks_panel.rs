use std::any::Any;

use peri_middlewares::hooks::types::{HookEvent, HookType, RegisteredHook};
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

/// Hook 事件描述信息
struct HookEventInfo {
    event: HookEvent,
    display_name: String,
    description: &'static str,
}

fn build_event_table() -> Vec<HookEventInfo> {
    let known = &[
        (HookEvent::PreToolUse, "PreToolUse", "Before tool execution"),
        (
            HookEvent::PostToolUse,
            "PostToolUse",
            "After tool execution",
        ),
        (
            HookEvent::PostToolUseFailure,
            "PostToolUseFailure",
            "After tool execution fails",
        ),
        (
            HookEvent::PermissionRequest,
            "PermissionRequest",
            "Before auto mode classifier decides",
        ),
        (
            HookEvent::UserPromptSubmit,
            "UserPromptSubmit",
            "When user submits a prompt",
        ),
        (
            HookEvent::SessionStart,
            "SessionStart",
            "When a new session starts",
        ),
        (HookEvent::SessionEnd, "SessionEnd", "When a session ends"),
        (HookEvent::Stop, "Stop", "When agent stops"),
        (
            HookEvent::StopFailure,
            "StopFailure",
            "When agent stops with failure",
        ),
        (
            HookEvent::PostToolBatch,
            "PostToolBatch",
            "When all parallel tools complete",
        ),
        (
            HookEvent::SubagentStart,
            "SubagentStart",
            "When a subagent starts",
        ),
        (
            HookEvent::SubagentStop,
            "SubagentStop",
            "When a subagent stops",
        ),
        (
            HookEvent::PreCompact,
            "PreCompact",
            "Before context compaction",
        ),
        (
            HookEvent::PostCompact,
            "PostCompact",
            "After context compaction",
        ),
        (
            HookEvent::Notification,
            "Notification",
            "When agent needs user input",
        ),
    ];
    known
        .iter()
        .map(|(e, n, d)| HookEventInfo {
            event: e.clone(),
            display_name: n.to_string(),
            description: d,
        })
        .collect()
}

/// 面板列表中的一行（事件 + 该事件下注册的 hooks 汇总）
#[derive(Debug, Clone)]
pub struct HookEventEntry {
    pub event: HookEvent,
    pub display_name: String,
    pub description: String,
    pub hook_count: usize,
    /// 该事件下注册的所有 hook 详情
    pub hooks: Vec<HookDetail>,
}

#[derive(Debug, Clone)]
pub struct HookDetail {
    pub hook_type: HookType,
    pub matcher: Option<String>,
    pub plugin_name: String,
}

#[derive(Clone)]
pub struct HooksPanel {
    /// 统一列表状态管理
    pub(crate) list: PanelList<HookEventEntry>,
}

/// 计算单个 hook 详情占几行（type+summary 行 + 可选 matcher 行 + plugin 行）
fn detail_lines(detail: &HookDetail) -> u16 {
    1 + if detail.matcher.is_some() { 1 } else { 0 } + 1
}

impl HooksPanel {
    pub fn new(registered_hooks: Vec<RegisteredHook>) -> Self {
        let event_table = build_event_table();

        // 收集所有出现的事件（去重），保持首次出现顺序
        let mut event_order: Vec<HookEvent> = Vec::new();
        for rh in &registered_hooks {
            if !event_order.contains(&rh.event) {
                event_order.push(rh.event.clone());
            }
        }

        let mut entries = Vec::new();

        for event in &event_order {
            let event_hooks: Vec<HookDetail> = registered_hooks
                .iter()
                .filter(|rh| &rh.event == event)
                .map(|rh| HookDetail {
                    hook_type: rh.hook.clone(),
                    matcher: rh.matcher.clone(),
                    plugin_name: rh.plugin_name.clone(),
                })
                .collect();

            if event_hooks.is_empty() {
                continue;
            }

            // 查找已知事件的描述
            let (display_name, description) = event_table
                .iter()
                .find(|info| &info.event == event)
                .map(|info| (info.display_name.clone(), info.description.to_string()))
                .unwrap_or_else(|| {
                    let name = match event {
                        HookEvent::Unknown(s) => s.clone(),
                        other => format!("{:?}", other),
                    };
                    (name, String::new())
                });

            entries.push(HookEventEntry {
                event: event.clone(),
                display_name,
                description,
                hook_count: event_hooks.len(),
                hooks: event_hooks,
            });
        }

        let mut list = PanelList::new();
        list.set_items(entries);
        Self { list }
    }

    pub fn cursor(&self) -> usize {
        self.list.cursor()
    }

    pub fn scroll_offset(&self) -> u16 {
        self.list.scroll_offset()
    }

    pub fn total(&self) -> usize {
        self.list.len()
    }

    pub fn total_hooks(&self) -> usize {
        self.list.items().iter().map(|e| e.hook_count).sum()
    }

    /// 当前选中的事件条目
    pub fn current_entry(&self) -> Option<&HookEventEntry> {
        self.list.selected()
    }

    /// 固定头部行数（统计行 + 提示行 + 空行）
    pub fn header_lines(&self) -> u16 {
        if self.list.is_empty() {
            2 // "none configured" + 空行
        } else {
            3 // 统计行 + 提示行 + 空行
        }
    }

    /// 当前光标 entry 对应的实际渲染行号（0-based）
    pub fn cursor_line(&self) -> u16 {
        let mut line = self.header_lines();
        // 每个 entry 前面有一个事件头行
        // 光标 entry 之前的所有 entry 各占 1 行（不展开）
        line += self.list.cursor() as u16;
        line
    }

    /// 当前光标 entry 展开后的总行数（事件头 + 详情 + 空行）
    pub fn expanded_lines(&self) -> u16 {
        let entry = match self.list.selected() {
            Some(e) => e,
            None => return 0,
        };
        let detail: u16 = entry.hooks.iter().map(detail_lines).sum();
        1 + detail + 1 // 事件头 + 详情行 + 空行
    }

    /// 整个面板的内容总行数
    pub fn total_content_lines(&self) -> u16 {
        let mut h = self.header_lines();
        for _entry in self.list.items() {
            h += 1; // 事件头行
        }
        // 加上当前展开的详情行
        if let Some(entry) = self.list.selected() {
            let detail: u16 = entry.hooks.iter().map(detail_lines).sum();
            h += detail + 1; // 详情行 + 空行
        }
        h
    }
}

/// 从 HookType 提取简短的类型标签（用于渲染）
pub fn hook_type_label(hook: &HookType) -> &'static str {
    match hook {
        HookType::Command { .. } => "command",
        HookType::Prompt { .. } => "prompt",
        HookType::Http { .. } => "http",
        HookType::Agent { .. } => "agent",
    }
}

/// 从 HookType 提取简短的执行内容摘要
pub fn hook_type_summary(hook: &HookType) -> String {
    match hook {
        HookType::Command { command, .. } => {
            let cmd: String = command.chars().take(40).collect();
            if command.chars().count() > 40 {
                format!("{}...", cmd)
            } else {
                cmd
            }
        }
        HookType::Prompt { prompt, .. } => {
            let p: String = prompt.chars().take(40).collect();
            if prompt.chars().count() > 40 {
                format!("{}...", p)
            } else {
                p
            }
        }
        HookType::Http { url, .. } => {
            let u: String = url.chars().take(40).collect();
            if url.chars().count() > 40 {
                format!("{}...", u)
            } else {
                u
            }
        }
        HookType::Agent { prompt, .. } => {
            let p: String = prompt.chars().take(40).collect();
            if prompt.chars().count() > 40 {
                format!("{}...", p)
            } else {
                p
            }
        }
    }
}

// ─── PanelComponent 实现 ──────────────────────────────────────────────────────

impl PanelComponent for HooksPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Hooks
    }

    fn handle_key(&mut self, input: Input, _ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;
        match input {
            Input {
                key: Key::Char('c'),
                ctrl: true,
                ..
            } => EventResult::NotConsumed,
            Input { key: Key::Esc, .. } => EventResult::ClosePanel,
            Input { key: Key::Up, .. } => {
                self.list.move_cursor(-1);
                self.list.ensure_visible(10);
                EventResult::Consumed
            }
            Input { key: Key::Down, .. } => {
                self.list.move_cursor(1);
                self.list.ensure_visible(10);
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
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
                .handle_mouse_click(mouse.row, mouse.column, area, 1);
        }
        EventResult::NotConsumed
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        self.total_content_lines().max(8)
    }

    fn render(&mut self, f: &mut Frame, app: &mut App, area: Rect) {
        crate::ui::main_ui::panels::hooks::render_hooks_panel(f, self, app, area);
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self, _lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                "\u{5bfc}\u{822a}".to_string(),
            ),
            ("Esc".to_string(), "\u{5173}\u{95ed}".to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;
    include!("hooks_panel_test.rs");
}
