//! 出口 readiness 与能力状态快照。
//!
//! 首个模型请求、面板与诊断消费同一有效能力集；状态类型不依赖 router 内部状态。

/// 能力在出口内的运行状态（readiness/面板/诊断共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// 可用；工具、资源、订阅与任务按声明面路由。
    Ready,
    /// 初始化或连接失败；该能力的全部面退出出口。
    Degraded { reason: String },
    /// 配置关闭；该能力不属于有效能力集。
    Disabled,
}

/// 单个能力的 readiness 快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityReadiness {
    pub capability: &'static str,
    pub status: CapabilityStatus,
    /// 声明且模型可见的工具数（`Ready` 时有效）。
    pub tools: usize,
    /// 声明资源面数量（静态资源 + 模板）。
    pub resources: usize,
}

/// 出口 readiness 快照：首个模型请求、面板与诊断消费同一有效能力集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterReadiness {
    /// 至少一项能力 ready。全部失败时首个请求已 fail-closed，不会出现 false 的可服务出口。
    pub ready: bool,
    /// 有效能力集（声明顺序）。
    pub effective: Vec<&'static str>,
    /// 逐能力状态（声明顺序，含 Disabled）。
    pub capabilities: Vec<CapabilityReadiness>,
}
