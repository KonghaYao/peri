#![allow(dead_code)]

use std::any::Any;

use ratatui::{layout::Rect, Frame};
use tui_textarea::Input;

use super::{
    agent_panel::AgentPanel, betas_panel::BetasPanel, config_panel::ConfigPanel,
    cron_state::CronPanel, hooks_panel::HooksPanel, login_panel::LoginPanel, mcp_panel::McpPanel,
    memory_panel::MemoryPanel, model_panel::ModelPanel, plugin_panel::PluginPanel,
    service_registry::ServiceRegistry, session_manager::SessionManager, status_panel::StatusPanel,
    tasks_panel::TasksPanel,
};
use crate::thread::ThreadBrowser;

// ─── PanelScope ─────────────────────────────────────────────────────────────

/// 面板作用域：Session 面板随 session 切换，Global 面板跨 session 保持
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelScope {
    Session,
    Global,
}

// ─── MutexGroup ─────────────────────────────────────────────────────────────

/// 互斥组：同组面板同时只能打开一个
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutexGroup {
    /// 模型/配置/登录面板互斥
    Settings,
    /// Agent/Hooks 面板互斥
    Agent,
    /// MCP/Cron/Plugin 面板互斥
    Tools,
    /// Status/Memory 面板互斥
    Info,
    /// ThreadBrowser 独占
    Thread,
}

// ─── PanelKind ──────────────────────────────────────────────────────────────

/// 穷举所有面板类型（编译时完整性保证）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Model,
    Login,
    Agent,
    Hooks,
    Config,
    ThreadBrowser,
    Mcp,
    Plugin,
    Cron,
    Status,
    Memory,
    Tasks,
    Betas,
}

impl PanelKind {
    /// 面板优先级（数值越小优先级越高，用于互斥决策）
    pub fn priority(&self) -> u8 {
        match self {
            PanelKind::Agent => 0,
            PanelKind::Hooks => 1,
            PanelKind::Model => 2,
            PanelKind::Login => 3,
            PanelKind::Config => 4,
            PanelKind::ThreadBrowser => 5,
            PanelKind::Mcp => 6,
            PanelKind::Plugin => 7,
            PanelKind::Cron => 8,
            PanelKind::Status => 9,
            PanelKind::Memory => 10,
            PanelKind::Tasks => 11,
            PanelKind::Betas => 12,
        }
    }

    /// 互斥组
    pub fn mutex_group(&self) -> MutexGroup {
        match self {
            PanelKind::Model | PanelKind::Login | PanelKind::Config => MutexGroup::Settings,
            PanelKind::Agent | PanelKind::Hooks => MutexGroup::Agent,
            PanelKind::Mcp | PanelKind::Plugin | PanelKind::Cron | PanelKind::Tasks => {
                MutexGroup::Tools
            }
            PanelKind::Status | PanelKind::Memory | PanelKind::Betas => MutexGroup::Info,
            PanelKind::ThreadBrowser => MutexGroup::Thread,
        }
    }

    /// 面板作用域
    pub fn scope(&self) -> PanelScope {
        match self {
            PanelKind::Model
            | PanelKind::Login
            | PanelKind::Agent
            | PanelKind::Hooks
            | PanelKind::Config
            | PanelKind::ThreadBrowser => PanelScope::Session,
            PanelKind::Mcp
            | PanelKind::Plugin
            | PanelKind::Cron
            | PanelKind::Status
            | PanelKind::Memory
            | PanelKind::Tasks
            | PanelKind::Betas => PanelScope::Global,
        }
    }
}

// ─── EventResult ────────────────────────────────────────────────────────────

/// 面板事件处理返回值
#[derive(Debug, PartialEq)]
pub enum EventResult {
    /// 事件已被消费，无需进一步处理
    Consumed,
    /// 事件未被消费，继续传递给后续处理器
    NotConsumed,
    /// 请求关闭当前面板
    ClosePanel,
    /// 请求打开另一个面板（用于面板间导航）
    OpenPanel(PanelKind),
    /// 请求打开指定 Thread（ThreadBrowser 专用）
    OpenThread(String),
}

// ─── PanelState ─────────────────────────────────────────────────────────────

/// 穷举存储面板实例（编译时完整性保证）
pub enum PanelState {
    Model(ModelPanel),
    Login(Box<LoginPanel>),
    Agent(AgentPanel),
    Hooks(HooksPanel),
    Config(Box<ConfigPanel>),
    ThreadBrowser(Box<ThreadBrowser>),
    Mcp(Box<McpPanel>),
    Plugin(Box<PluginPanel>),
    Cron(CronPanel),
    Status(StatusPanel),
    Memory(MemoryPanel),
    Tasks(TasksPanel),
    Betas(BetasPanel),
}

// ─── panel_dispatch! 宏 ────────────────────────────────────────────────────
//
// 13 变体 × 13 方法 = ~169 个 match arm 的样板代码由宏统一展开。
//
// 设计要点：
// - `PanelState::*` 变体中部分内部是 `Box<T>`（Login/Config/ThreadBrowser/Mcp/Plugin），
//   其余是裸 `T`。两类处理差异：
//     * 方法调用（trait method）→ `Box<T>: Deref<Target=T>` 自动穿透，`$body` 对所有
//       变体统一可用
//     * `&dyn Any` 强转 → deref coercion 不会发生（unsize 优先于 deref），
//       `&Box<T> as &dyn Any` 得到 `TypeId::of::<Box<T>>()`，downcast 会失败。
//       必须对 Box 变体显式 `.as_ref()` / `.as_mut()` 解引用后再强转。
//   因此 `any:ref` / `any:mut` 分支对裸变体和 Box 变体使用不同表达式。
// - 穷举所有 13 个变体，新增 PanelState 变体时编译器会强制更新宏的每一个展开点
//   （enum 穷举保证不丢失）。
//
// 宏分支：
//   1. `kind`      — 对应 `PanelState::kind()`，每个 arm 返回对应的 `PanelKind` 常量
//   2. `any:ref`   — 对应 `as_any_ref()`，Box 变体用 `.as_ref()`，裸变体直接强转
//   3. `any:mut`   — 对应 `as_any_mut()`，Box 变体用 `.as_mut()`，裸变体直接强转
//   4. 通用 `$body` — trait 方法调用分发，所有 arm 共用同一表达式
//
// 调用约定：调用方负责 `use super::panel_component::PanelComponent;` 引入 trait。
macro_rules! panel_dispatch {
    // kind()：每个变体返回固定 PanelKind 常量
    (kind: $state:expr) => {
        match $state {
            PanelState::Model(_) => PanelKind::Model,
            PanelState::Login(_) => PanelKind::Login,
            PanelState::Agent(_) => PanelKind::Agent,
            PanelState::Hooks(_) => PanelKind::Hooks,
            PanelState::Config(_) => PanelKind::Config,
            PanelState::ThreadBrowser(_) => PanelKind::ThreadBrowser,
            PanelState::Mcp(_) => PanelKind::Mcp,
            PanelState::Plugin(_) => PanelKind::Plugin,
            PanelState::Cron(_) => PanelKind::Cron,
            PanelState::Status(_) => PanelKind::Status,
            PanelState::Memory(_) => PanelKind::Memory,
            PanelState::Tasks(_) => PanelKind::Tasks,
            PanelState::Betas(_) => PanelKind::Betas,
        }
    };
    // as_any_ref()：Box 变体用 .as_ref() 显式解引用，避免 TypeId 记成 Box<T>
    (any:ref $state:expr) => {
        match $state {
            PanelState::Model(p) => p as &dyn Any,
            PanelState::Login(p) => p.as_ref() as &dyn Any,
            PanelState::Agent(p) => p as &dyn Any,
            PanelState::Hooks(p) => p as &dyn Any,
            PanelState::Config(p) => p.as_ref() as &dyn Any,
            PanelState::ThreadBrowser(p) => p.as_ref() as &dyn Any,
            PanelState::Mcp(p) => p.as_ref() as &dyn Any,
            PanelState::Plugin(p) => p.as_ref() as &dyn Any,
            PanelState::Cron(p) => p as &dyn Any,
            PanelState::Status(p) => p as &dyn Any,
            PanelState::Memory(p) => p as &dyn Any,
            PanelState::Tasks(p) => p as &dyn Any,
            PanelState::Betas(p) => p as &dyn Any,
        }
    };
    // as_any_mut()：Box 变体用 .as_mut() 显式解引用
    (any:mut $state:expr) => {
        match $state {
            PanelState::Model(p) => p as &mut dyn Any,
            PanelState::Login(p) => p.as_mut() as &mut dyn Any,
            PanelState::Agent(p) => p as &mut dyn Any,
            PanelState::Hooks(p) => p as &mut dyn Any,
            PanelState::Config(p) => p.as_mut() as &mut dyn Any,
            PanelState::ThreadBrowser(p) => p.as_mut() as &mut dyn Any,
            PanelState::Mcp(p) => p.as_mut() as &mut dyn Any,
            PanelState::Plugin(p) => p.as_mut() as &mut dyn Any,
            PanelState::Cron(p) => p as &mut dyn Any,
            PanelState::Status(p) => p as &mut dyn Any,
            PanelState::Memory(p) => p as &mut dyn Any,
            PanelState::Tasks(p) => p as &mut dyn Any,
            PanelState::Betas(p) => p as &mut dyn Any,
        }
    };
    // 通用方法分发：所有变体共用 $body（$p 为绑定名）
    ($state:expr, $p:ident, $body:expr) => {
        match $state {
            PanelState::Model($p) => $body,
            PanelState::Login($p) => $body,
            PanelState::Agent($p) => $body,
            PanelState::Hooks($p) => $body,
            PanelState::Config($p) => $body,
            PanelState::ThreadBrowser($p) => $body,
            PanelState::Mcp($p) => $body,
            PanelState::Plugin($p) => $body,
            PanelState::Cron($p) => $body,
            PanelState::Status($p) => $body,
            PanelState::Memory($p) => $body,
            PanelState::Tasks($p) => $body,
            PanelState::Betas($p) => $body,
        }
    };
}

impl PanelState {
    /// 获取面板类型
    pub fn kind(&self) -> PanelKind {
        panel_dispatch!(kind: self)
    }

    /// Any downcast（不可变引用）
    ///
    /// 注意：Box 变体必须经 `.as_ref()` 解引用后再强转，否则 TypeId 会记成 `Box<T>`
    /// 而非 `T`，导致 `downcast_ref::<T>()` 失败。
    pub fn as_any_ref(&self) -> &dyn Any {
        panel_dispatch!(any:ref self)
    }

    /// Any downcast（可变引用）
    pub fn as_any_mut(&mut self) -> &mut dyn Any {
        panel_dispatch!(any:mut self)
    }

    /// 委托渲染到对应面板组件
    pub fn render(&mut self, f: &mut Frame, app: &mut super::App, area: Rect) {
        use super::panel_component::PanelComponent;
        panel_dispatch!(self, p, p.render(f, app, area))
    }

    /// 委托获取期望面板高度
    pub fn desired_height(&self, screen_height: u16, screen_width: u16) -> u16 {
        use super::panel_component::PanelComponent;
        panel_dispatch!(self, p, p.desired_height(screen_height, screen_width))
    }

    /// 委托获取快捷键提示
    pub fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        use super::panel_component::PanelComponent;
        panel_dispatch!(self, p, p.status_bar_hints(lc))
    }
}

// ─── PanelContext ───────────────────────────────────────────────────────────

/// 面板处理器上下文：解耦面板与 App 的借用冲突
pub struct PanelContext<'a> {
    pub services: &'a mut ServiceRegistry,
    pub session_mgr: &'a mut SessionManager,
    pub acp_client: Option<crate::acp_client::AcpTuiClient>,
}

impl PanelContext<'_> {
    // `sync_acp_config` 已移除：TUI 与 ACP Server 共享同一 `Arc<RwLock<PeriConfig>>`，
    // 写入即时传播，无需手动同步。
}

// ─── PanelManager ───────────────────────────────────────────────────────────

/// 面板管理器：集中管理面板的打开/关闭/查询和事件分发
pub struct PanelManager {
    active: Option<PanelState>,
}

impl PanelManager {
    pub fn new() -> Self {
        Self { active: None }
    }

    /// 获取当前激活面板的类型
    pub fn active_kind(&self) -> Option<PanelKind> {
        self.active.as_ref().map(|s| s.kind())
    }

    /// 获取当前激活面板的不可变引用
    pub fn active_state(&self) -> Option<&PanelState> {
        self.active.as_ref()
    }

    /// 获取当前激活面板的可变引用
    pub fn active_state_mut(&mut self) -> Option<&mut PanelState> {
        self.active.as_mut()
    }

    /// 取出当前激活面板（用于需要 &mut App 的渲染场景，避免双重可变借用）
    pub fn take_active(&mut self) -> Option<PanelState> {
        self.active.take()
    }

    /// 放回面板（配合 take_active 使用）
    pub fn put_active(&mut self, state: PanelState) {
        self.active = Some(state);
    }

    /// 检查指定类型的面板是否激活
    pub fn is_active(&self, kind: PanelKind) -> bool {
        self.active_kind() == Some(kind)
    }

    /// 检查是否有任何面板打开
    pub fn is_any_open(&self) -> bool {
        self.active.is_some()
    }

    /// 打开面板：自动关闭同作用域的前一面板，返回被关闭的面板
    pub fn open(&mut self, state: PanelState) -> Option<PanelState> {
        self.active.replace(state)
    }

    /// 关闭当前面板，返回被关闭的面板
    pub fn close(&mut self) -> Option<PanelState> {
        self.active.take()
    }

    /// 仅当指定类型的面板激活时才关闭
    pub fn close_if(&mut self, kind: PanelKind) -> Option<PanelState> {
        if self.is_active(kind) {
            self.close()
        } else {
            None
        }
    }

    /// 类型安全地获取面板的不可变引用
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.active.as_ref()?.as_any_ref().downcast_ref::<T>()
    }

    /// 类型安全地获取面板的可变引用
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.active.as_mut()?.as_any_mut().downcast_mut::<T>()
    }

    /// 分发按键事件到当前激活面板
    pub fn dispatch_key(&mut self, input: Input, ctx: &mut PanelContext<'_>) -> EventResult {
        use super::panel_component::PanelComponent;
        let Some(state) = self.active.as_mut() else {
            return EventResult::NotConsumed;
        };
        panel_dispatch!(state, p, p.handle_key(input, ctx))
    }

    /// 分发粘贴事件到当前激活面板
    pub fn dispatch_paste(&mut self, text: &str, ctx: &mut PanelContext<'_>) -> EventResult {
        use super::panel_component::PanelComponent;
        let Some(state) = self.active.as_mut() else {
            return EventResult::NotConsumed;
        };
        panel_dispatch!(state, p, p.handle_paste(text, ctx))
    }

    /// 分发滚动事件到当前激活面板
    pub fn dispatch_scroll(&mut self, lines: i16, ctx: &mut PanelContext<'_>) -> EventResult {
        use super::panel_component::PanelComponent;
        let Some(state) = self.active.as_mut() else {
            return EventResult::NotConsumed;
        };
        panel_dispatch!(state, p, p.handle_scroll(lines, ctx))
    }

    /// 分发鼠标事件到当前激活面板
    pub fn dispatch_mouse(
        &mut self,
        mouse: ratatui::crossterm::event::MouseEvent,
        area: ratatui::layout::Rect,
        ctx: &mut PanelContext<'_>,
    ) -> EventResult {
        use super::panel_component::PanelComponent;
        let Some(state) = self.active.as_mut() else {
            return EventResult::NotConsumed;
        };
        panel_dispatch!(state, p, p.handle_mouse(mouse, area, ctx))
    }

    /// 获取当前激活面板的快捷键提示
    pub fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        use super::panel_component::PanelComponent;
        let Some(state) = self.active.as_ref() else {
            return Vec::new();
        };
        panel_dispatch!(state, p, p.status_bar_hints(lc))
    }

    /// 查询当前激活面板的期望高度
    ///
    /// `Some(match state { ... })` 形态：宏展开后表达式被 `Some()` 包裹。
    pub fn dispatch_desired_height(&self, screen_height: u16, screen_width: u16) -> Option<u16> {
        use super::panel_component::PanelComponent;
        let state = self.active.as_ref()?;
        Some(panel_dispatch!(
            state,
            p,
            p.desired_height(screen_height, screen_width)
        ))
    }

    /// 分发绝对滚动偏移到当前激活面板（滚动条拖拽）
    pub fn dispatch_set_scroll_offset(&mut self, offset: u16) {
        use super::panel_component::PanelComponent;
        let Some(state) = self.active.as_mut() else {
            return;
        };
        panel_dispatch!(state, p, p.set_scroll_offset(offset))
    }
}

impl Default for PanelManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "panel_manager_test.rs"]
mod tests;
