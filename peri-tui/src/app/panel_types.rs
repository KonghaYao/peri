//! v2 面板系统共享类型。

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
    AskUser,
    Theme,
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
