//! Tool Search 延迟加载模块
//!
//! 将非核心工具（MCP 工具、Cron 工具等）从 LLM API 调用中移除，
//! 通过 SearchExtraTools + ExecuteExtraTool 两个元工具实现按需发现和代理执行。

pub mod artifact_client;
pub mod artifact_tool;
pub mod core_tools;
pub mod execute_tool;
pub mod keyword_search;
pub mod middleware;
pub mod search_tool;
pub mod tool_index;

pub use core_tools::{
    resolve_effective_tool_name, EXECUTE_EXTRA_TOOL_NAME, EXTRA_TOOL_NAME_FIELD,
    EXTRA_TOOL_PARAMS_FIELD, SEARCH_EXTRA_TOOLS_NAME,
};
pub use execute_tool::{ExecuteExtraTool, ExecuteExtraToolResolver};
pub use middleware::ToolSearchMiddleware;
pub use search_tool::SearchExtraTools;
pub use tool_index::{SearchResult, ToolSearchIndex};
