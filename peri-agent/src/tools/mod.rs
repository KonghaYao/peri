pub mod invocation;
pub use invocation::{
    normalize_params, CanonicalToolInvocation, DirectToolInvocationResolver, ToolInvocationResolver,
};

/// 工具契约类型（事实源 peri-acp-types::tools；本模块保留 re-export 保兼容，
/// 跨层调用面统一经 `peri_acp_types::tools` 解析）。
pub use peri_acp_types::tools::{
    BaseTool, ContextRetention, ToolContext, ToolDefinition, ToolDescription,
};
