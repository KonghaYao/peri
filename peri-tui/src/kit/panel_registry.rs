//! Panel registry——`PanelKind` 元数据 + open/close/toggle 操作。
//!
//! 这是 kit 路径"面板系统"的入口——所有 14 种面板的快捷键映射、标题、互斥组
//! 规则、原子操作都集中在这里。
//!
//! ## 互斥组语义
//!
//! 同 `MutexGroup` 面板不可同时打开（参见 `panel_types.rs::mutex_group`）。
//! `open_panel(kind)` 在打开新面板前会关闭同组其他面板——这保证栈中
//! `Vec<PanelKind>` 不会同时含两个同组面板。

use peri_theme::atoms::THEME_ATOM;
use ratatui::{
    style::Style,
    widgets::{Scrollbar, ScrollbarOrientation},
};
use ratatui_kit::{
    components::scroll_view::{ScrollbarVisibility, Scrollbars},
    crossterm::event::KeyCode,
    prelude::*,
    ratatui::layout::Constraint,
};

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
use crate::kit::panels::{
    agent::AgentPanel, ask_user::AskUserPanel, betas::BetasPanel, config::ConfigPanel,
    cron::CronPanel, hooks::HooksPanel, login::LoginPanel, mcp::McpPanel, memory::MemoryPanel,
    model::ModelPanel, plugin::PluginPanel, status::StatusPanel, tasks::TasksPanel,
    theme::ThemePanel, thread_browser::ThreadBrowserPanel, workflow::WorkflowPanel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelScope {
    Session,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutexGroup {
    Settings,
    Agent,
    Tools,
    Info,
    Thread,
    AskUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSize {
    Fill,
    Length(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelLayout {
    pub width: PanelSize,
    pub height: PanelSize,
}

impl PanelLayout {
    pub const fn fixed(width: u16, height: u16) -> Self {
        Self {
            width: PanelSize::Length(width),
            height: PanelSize::Length(height),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PanelMeta {
    pub kind: PanelKind,
    pub title: &'static str,
    /// 触发面板的快捷键字母（小写）。`KeyCode::Char(letter)` + Ctrl。
    pub shortcut_letter: char,
    pub slash_command: &'static str,
    pub description: &'static str,
    pub priority: u8,
    pub mutex_group: MutexGroup,
    pub scope: PanelScope,
    pub layout: PanelLayout,
    pub render: fn() -> AnyElement<'static>,
}

fn render_model_panel() -> AnyElement<'static> {
    element!(ModelPanel()).into()
}

fn render_login_panel() -> AnyElement<'static> {
    element!(LoginPanel()).into()
}

fn render_agent_panel() -> AnyElement<'static> {
    element!(AgentPanel()).into()
}

fn render_hooks_panel() -> AnyElement<'static> {
    element!(HooksPanel()).into()
}

fn render_config_panel() -> AnyElement<'static> {
    element!(ConfigPanel()).into()
}

fn render_thread_browser_panel() -> AnyElement<'static> {
    element!(ThreadBrowserPanel()).into()
}

fn render_mcp_panel() -> AnyElement<'static> {
    element!(McpPanel()).into()
}

fn render_plugin_panel() -> AnyElement<'static> {
    element!(PluginPanel()).into()
}

fn render_cron_panel() -> AnyElement<'static> {
    element!(CronPanel()).into()
}

fn render_status_panel() -> AnyElement<'static> {
    element!(StatusPanel()).into()
}

fn render_memory_panel() -> AnyElement<'static> {
    element!(MemoryPanel()).into()
}

fn render_tasks_panel() -> AnyElement<'static> {
    element!(TasksPanel()).into()
}

fn render_betas_panel() -> AnyElement<'static> {
    element!(BetasPanel()).into()
}

fn render_workflow_panel() -> AnyElement<'static> {
    element!(WorkflowPanel()).into()
}

fn render_ask_user_panel() -> AnyElement<'static> {
    element!(AskUserPanel()).into()
}

fn render_theme_panel() -> AnyElement<'static> {
    element!(ThemePanel()).into()
}

/// 所有 14 面板的元数据。
///
/// 快捷键分配（避开 Ctrl+C 全局 quit）：
/// - Ctrl+M = Model（替代 legacy cycle model）
/// - Ctrl+T = ThreadBrowser
/// - Ctrl+R = Cron
/// - Ctrl+S = Status
/// - Ctrl+L = Login
/// - Ctrl+H = Hooks
/// - Ctrl+J = Tasks
/// - Ctrl+B = Betas
/// - Ctrl+P = Plugin
/// - Ctrl+G = Agent
/// - Ctrl+F = Config
/// - Ctrl+W = Workflow
/// - Ctrl+N = Memory
/// - Ctrl+X = Mcp
pub const PANELS: &[PanelMeta] = &[
    PanelMeta {
        kind: PanelKind::Model,
        title: "Model",
        shortcut_letter: 'm',
        slash_command: "model",
        description: "Model alias selection",
        priority: 2,
        mutex_group: MutexGroup::Settings,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_model_panel,
    },
    PanelMeta {
        kind: PanelKind::Login,
        title: "Login",
        shortcut_letter: 'l',
        slash_command: "login",
        description: "Provider credentials",
        priority: 3,
        mutex_group: MutexGroup::Settings,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_login_panel,
    },
    PanelMeta {
        kind: PanelKind::Agent,
        title: "Agent",
        shortcut_letter: 'g',
        slash_command: "agent",
        description: "Subagent definitions",
        priority: 0,
        mutex_group: MutexGroup::Agent,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_agent_panel,
    },
    PanelMeta {
        kind: PanelKind::Hooks,
        title: "Hooks",
        shortcut_letter: 'h',
        slash_command: "hooks",
        description: "Hook events",
        priority: 1,
        mutex_group: MutexGroup::Agent,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_hooks_panel,
    },
    PanelMeta {
        kind: PanelKind::Config,
        title: "Config",
        shortcut_letter: 'f',
        slash_command: "config",
        description: "PeriConfig editor",
        priority: 4,
        mutex_group: MutexGroup::Settings,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_config_panel,
    },
    PanelMeta {
        kind: PanelKind::ThreadBrowser,
        title: "Threads",
        shortcut_letter: 't',
        slash_command: "threads",
        description: "Thread history browser",
        priority: 5,
        mutex_group: MutexGroup::Thread,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_thread_browser_panel,
    },
    PanelMeta {
        kind: PanelKind::Mcp,
        title: "MCP",
        shortcut_letter: 'x',
        slash_command: "mcp",
        description: "MCP server pool",
        priority: 6,
        mutex_group: MutexGroup::Tools,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(60, 18),
        render: render_mcp_panel,
    },
    PanelMeta {
        kind: PanelKind::Plugin,
        title: "Plugin",
        shortcut_letter: '\0',
        slash_command: "plugin",
        description: "Installed plugins",
        priority: 7,
        mutex_group: MutexGroup::Tools,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(80, 24),
        render: render_plugin_panel,
    },
    PanelMeta {
        kind: PanelKind::Cron,
        title: "Cron",
        shortcut_letter: 'r',
        slash_command: "cron",
        description: "Scheduled tasks",
        priority: 8,
        mutex_group: MutexGroup::Tools,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(60, 18),
        render: render_cron_panel,
    },
    PanelMeta {
        kind: PanelKind::Status,
        title: "Status",
        shortcut_letter: 's',
        slash_command: "status",
        description: "Service snapshot",
        priority: 9,
        mutex_group: MutexGroup::Info,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(60, 18),
        render: render_status_panel,
    },
    PanelMeta {
        kind: PanelKind::Memory,
        title: "Memory",
        shortcut_letter: 'n',
        slash_command: "memory",
        description: "Persisted memory",
        priority: 10,
        mutex_group: MutexGroup::Info,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(60, 18),
        render: render_memory_panel,
    },
    PanelMeta {
        kind: PanelKind::Tasks,
        title: "Tasks",
        shortcut_letter: 'j',
        slash_command: "tasks",
        description: "Background tasks",
        priority: 11,
        mutex_group: MutexGroup::Tools,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(60, 18),
        render: render_tasks_panel,
    },
    PanelMeta {
        kind: PanelKind::Betas,
        title: "Betas",
        shortcut_letter: 'b',
        slash_command: "betas",
        description: "Feature flags",
        priority: 12,
        mutex_group: MutexGroup::Info,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(60, 18),
        render: render_betas_panel,
    },
    PanelMeta {
        kind: PanelKind::Workflow,
        title: "Workflow",
        shortcut_letter: 'w',
        slash_command: "workflows",
        description: "Workflow runs",
        priority: 13,
        mutex_group: MutexGroup::Tools,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(90, 14),
        render: render_workflow_panel,
    },
    PanelMeta {
        kind: PanelKind::AskUser,
        title: "Ask User",
        shortcut_letter: '\0',
        slash_command: "",
        description: "Agent user questions (auto-open)",
        priority: 14,
        mutex_group: MutexGroup::AskUser,
        scope: PanelScope::Session,
        layout: PanelLayout::fixed(60, 18),
        render: render_ask_user_panel,
    },
    PanelMeta {
        kind: PanelKind::Theme,
        title: "Theme",
        shortcut_letter: 'e',
        slash_command: "theme",
        description: "Color theme selection",
        priority: 15,
        mutex_group: MutexGroup::Settings,
        scope: PanelScope::Global,
        layout: PanelLayout::fixed(50, 24),
        render: render_theme_panel,
    },
];

pub fn slash_command_for_panel(kind: PanelKind) -> &'static str {
    meta(kind)
        .expect("all PanelKind variants must be registered")
        .slash_command
}

pub fn panel_for_slash_command(command: &str) -> Option<PanelKind> {
    let normalized = command.trim_start_matches('/').to_ascii_lowercase();
    // /history、/resume、/his 都是 /threads（Thread Browser 面板）的别名。
    // ACP server 将 "history"/"resume" 作为远程 command 下发，但 TUI 应映射为面板打开。
    if normalized == "history" || normalized == "resume" || normalized == "his" {
        return Some(PanelKind::ThreadBrowser);
    }
    PANELS
        .iter()
        .find(|m| m.slash_command == normalized)
        .map(|m| m.kind)
}

pub fn panel_title(kind: PanelKind) -> String {
    let key = match kind {
        PanelKind::Model => "panel-title-model",
        PanelKind::Login => "panel-title-login",
        PanelKind::Agent => "panel-title-agent",
        PanelKind::Hooks => "panel-title-hooks",
        PanelKind::Config => "panel-title-config",
        PanelKind::ThreadBrowser => "panel-title-threads",
        PanelKind::Mcp => "panel-title-mcp",
        PanelKind::Plugin => "panel-title-plugin",
        PanelKind::Cron => "panel-title-cron",
        PanelKind::Status => "panel-title-status",
        PanelKind::Memory => "panel-title-memory",
        PanelKind::Tasks => "panel-title-tasks",
        PanelKind::Betas => "panel-title-betas",
        PanelKind::Workflow => "panel-title-workflow",
        PanelKind::AskUser => "panel-title-ask-user",
        PanelKind::Theme => "panel-title-theme",
    };
    format!(" {} ", i18n::tr(key))
}

pub fn panel_description(kind: PanelKind) -> String {
    let key = match kind {
        PanelKind::Model => "panel-desc-model",
        PanelKind::Login => "panel-desc-login",
        PanelKind::Agent => "panel-desc-agent",
        PanelKind::Hooks => "panel-desc-hooks",
        PanelKind::Config => "panel-desc-config",
        PanelKind::ThreadBrowser => "panel-desc-threads",
        PanelKind::Mcp => "panel-desc-mcp",
        PanelKind::Plugin => "panel-desc-plugin",
        PanelKind::Cron => "panel-desc-cron",
        PanelKind::Status => "panel-desc-status",
        PanelKind::Memory => "panel-desc-memory",
        PanelKind::Tasks => "panel-desc-tasks",
        PanelKind::Betas => "panel-desc-betas",
        PanelKind::Workflow => "panel-desc-workflow",
        PanelKind::AskUser => "panel-desc-ask-user",
        PanelKind::Theme => "panel-desc-theme",
    };
    i18n::tr(key)
}

pub fn panel_layout(kind: PanelKind) -> PanelLayout {
    meta(kind)
        .expect("all PanelKind variants must be registered")
        .layout
}

pub fn panel_constraint(size: PanelSize) -> Constraint {
    match size {
        PanelSize::Fill => Constraint::Fill(1),
        PanelSize::Length(value) => Constraint::Length(value),
    }
}

pub fn render(kind: PanelKind) -> Option<AnyElement<'static>> {
    meta(kind).map(|m| (m.render)())
}

/// 查找面板元数据。未注册返回 None。
pub fn meta(kind: PanelKind) -> Option<&'static PanelMeta> {
    PANELS.iter().find(|m| m.kind == kind)
}

/// 按快捷键字母反查 PanelKind。未注册返回 None。
pub fn from_shortcut(letter: char) -> Option<PanelKind> {
    let lower = letter.to_ascii_lowercase();
    PANELS
        .iter()
        .find(|m| m.shortcut_letter == lower)
        .map(|m| m.kind)
}

/// 将 crossterm 的 Ctrl+Char 事件映射到 PanelKind。
///
/// 调用方已确认 Ctrl 修饰键按下。返回 None 表示该字母未注册任何面板
/// （也可能是 Ctrl+C/K 等保留快捷键）。
pub fn from_key_code(code: KeyCode) -> Option<PanelKind> {
    if let KeyCode::Char(ch) = code {
        from_shortcut(ch)
    } else {
        None
    }
}

// ── 面板栈操作（mutates OPEN_PANELS / ACTIVE_PANEL atoms） ──────────────────

/// 打开面板：应用互斥组规则后压入栈顶并设为 ACTIVE_PANEL。
///
/// - 若面板已在栈中：把它移到栈顶（不重复 push）。
/// - 若同 MutexGroup 有其他面板：先关闭它们。
/// - 若面板不在栈中：push 到栈尾（栈顶）。
pub fn open_panel(kind: PanelKind) {
    let open_atom = OPEN_PANELS.state();
    let active_atom = ACTIVE_PANEL.state();

    let group = meta(kind)
        .expect("all PanelKind variants must be registered")
        .mutex_group;
    let mut current = open_atom.read().clone();

    // 关闭同 MutexGroup 的其他面板（除 kind 自身）
    current.retain(|k| {
        let item_group = meta(*k)
            .expect("all PanelKind variants must be registered")
            .mutex_group;
        *k == kind || item_group != group
    });

    // 若 kind 已在栈中，先移除（稍后 push 到栈顶）
    current.retain(|k| *k != kind);

    // push 到栈顶
    current.push(kind);

    *open_atom.write() = current;
    *active_atom.write() = Some(kind);
}

/// 关闭栈顶（ACTIVE_PANEL）面板，弹出后新的栈顶成为 active。
///
/// 返回被关闭的 PanelKind（若有），调用方可用于日志/状态反馈。
pub fn close_active_panel() -> Option<PanelKind> {
    let open_atom = OPEN_PANELS.state();
    let active_atom = ACTIVE_PANEL.state();

    let mut current = open_atom.read().clone();
    let closed = current.pop();
    let next_active = current.last().copied();
    *open_atom.write() = current;
    *active_atom.write() = next_active;
    closed
}

/// 关闭指定面板：从栈中移除，删除成功后统一重算 ACTIVE_PANEL 为新的栈顶。
pub fn close_panel(kind: PanelKind) -> bool {
    let open_atom = OPEN_PANELS.state();
    let active_atom = ACTIVE_PANEL.state();

    let mut current = open_atom.read().clone();
    let before_len = current.len();
    current.retain(|k| *k != kind);
    let removed = current.len() < before_len;
    if removed {
        let next_active = current.last().copied();
        *open_atom.write() = current;
        *active_atom.write() = next_active;
    }
    removed
}

/// Toggle：若已打开则关闭，否则打开。返回操作后的最终状态（true=已打开）。
pub fn toggle_panel(kind: PanelKind) -> bool {
    let is_open = OPEN_PANELS.state().read().contains(&kind);
    if is_open {
        close_panel(kind);
        false
    } else {
        open_panel(kind);
        true
    }
}

/// 关闭所有面板（清空栈）。
pub fn close_all_panels() {
    *OPEN_PANELS.state().write() = Vec::new();
    *ACTIVE_PANEL.state().write() = None;
}

// ── S16：per-widgets 风格干净滚动条（空格 + bg 色，无 █ 缝隙）──

/// 创建 peri-widgets 风格的垂直滚动条配置：
/// thumb 纯色块（fg==bg 让 █ 退化为纯背景色），track 透明。
/// 注意：ratatui-kit ScrollView 内部 `.orientation()` 会把 thumb_symbol 重置为
/// DEFAULT_VERTICAL 的 "█"，因此通过 fg==bg 同色方案实现"空白反色"视觉效果。
pub fn clean_scrollbars() -> Scrollbars<'static> {
    let thumb_bg = THEME_ATOM.state().read().semantic.text.dim;
    Scrollbars {
        vertical_scrollbar: Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol(" ")
            .thumb_style(Style::default().fg(thumb_bg).bg(thumb_bg))
            .track_symbol(None)
            .begin_symbol(Some("▲"))
            .begin_style(
                Style::default()
                    .fg(THEME_ATOM.state().read().semantic.text.muted)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )
            .end_symbol(Some("▼"))
            .end_style(
                Style::default()
                    .fg(THEME_ATOM.state().read().semantic.text.muted)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
        vertical_scrollbar_visibility: ScrollbarVisibility::Automatic,
        ..Scrollbars::default()
    }
}

#[cfg(test)]
#[path = "panel_registry_test.rs"]
mod tests;
