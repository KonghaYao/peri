use serde::{Deserialize, Serialize};

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
}
