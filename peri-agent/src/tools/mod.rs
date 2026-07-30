use serde::{Deserialize, Serialize};

pub mod invocation;
pub use invocation::{
    normalize_params, CanonicalToolInvocation, DirectToolInvocationResolver, ToolInvocationResolver,
};

/// 工具上下文保留策略（用于 Compact 决策）
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

/// 工具定义（JSON Schema 格式参数描述）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
}

/// 工具只读上下文（借用 state，零 clone）
///
/// 通过 `BaseTool::invoke` 的第二个参数传入。工具可读取 messages 和 cwd，
/// 但不能修改 state（避免绕过 dispatch_tools 统一写入语义）。
pub struct ToolContext<'a> {
    /// 当前对话历史（只读引用，借用 state.messages）
    pub messages: &'a [crate::messages::BaseMessage],
    /// 当前工作目录
    pub cwd: &'a str,
}

impl<'a> ToolContext<'a> {
    pub fn new(messages: &'a [crate::messages::BaseMessage], cwd: &'a str) -> Self {
        Self { messages, cwd }
    }
}

/// BaseTool trait - 对齐 LangChain Python BaseTool
///
/// 所有工具必须实现此 trait，不再依赖 langchain-rust::tools::Tool。
#[async_trait::async_trait]
pub trait BaseTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    /// 返回完整工具定义（默认实现，组合 name/description/parameters）
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    /// 执行工具，输入为 JSON Value
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;

    /// 工具调用的外层超时时间。None 表示不设外层超时（工具自行管理超时）。
    /// 默认 120s，适用于 Read/Edit/Glob 等快速操作。Agent/Bash 等工具应返回
    /// None，因为它们内部已有超时机制或需要长时间运行。
    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(120))
    }

    /// 工具声明的别名列表。当 LLM 输出的工具名匹配这些别名（大小写无关）时，
    /// 由 resolve_tool() 解析到本工具。典型用例：BashTool → aliases=["Shell"]。
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// 工具输出的默认截断长度（字符数）。None 表示不截断。
    fn output_char_limit(&self) -> Option<usize> {
        None
    }

    /// 工具输出是否偏向落盘而非内联返回。
    fn prefers_persist(&self) -> bool {
        false
    }

    /// 工具在上下文压缩中的保留策略。
    ///
    /// 默认返回 `ContextRetention::Preserve`——未显式标注的工具绝对不会被压缩。
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::Preserve
    }

    /// 是否直接出现在 LLM 的 tools 参数中（无需经过 SearchExtraTools 发现）。
    /// 默认 `false`（安全默认值：新工具默认为 deferred）。
    fn is_direct(&self) -> bool {
        false
    }
}
