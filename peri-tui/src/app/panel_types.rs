//! v2 面板系统共享类型。

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
    Workflow,
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
            PanelKind::Workflow => 13,
        }
    }

    /// 互斥组
    pub fn mutex_group(&self) -> MutexGroup {
        match self {
            PanelKind::Model | PanelKind::Login | PanelKind::Config => MutexGroup::Settings,
            PanelKind::Agent | PanelKind::Hooks => MutexGroup::Agent,
            PanelKind::Mcp
            | PanelKind::Plugin
            | PanelKind::Cron
            | PanelKind::Tasks
            | PanelKind::Workflow => MutexGroup::Tools,
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
            | PanelKind::Betas
            | PanelKind::Workflow => PanelScope::Global,
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
