use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::json;

#[path = "agent_result/agent_result_tool.rs"]
mod agent_result_tool;
pub use agent_result_tool::AgentResultTool;
