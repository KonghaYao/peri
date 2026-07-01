// (S13c-4c) 已删除 map_acp_event（AcpEvent → AgentEvent 映射，仅 legacy agent_ops 用）。
// 保留 LlmProvider re-export：launch/cli_print/acp_server/acp_stdio 等仍引用 `app::agent::LlmProvider`。
pub use super::provider::LlmProvider;
