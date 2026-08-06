//! 工具契约类型（自 peri-agent 迁入，`peri-agent::tools` 保留 re-export）。

use serde::{Deserialize, Serialize};

/// 工具定义（JSON Schema 格式参数描述）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
}

/// 工具上下文保留策略（用于 Compact 决策；自 peri-agent 迁入，
/// `peri-agent::tools::ContextRetention` 保留 re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextRetention {
    /// 必须完整保留（用户回答、目标、任务状态工具）
    Preserve,
    /// 后续控制流依赖的状态（后续可能降级但不是现在）
    StateBearing,
    /// 副作用已完成的收据（只需保留摘要/状态）
    SideEffectReceipt,
    /// 可从磁盘/网络重新获取
    Recomputable,
}
