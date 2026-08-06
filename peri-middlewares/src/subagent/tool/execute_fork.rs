//! SubAgent 同步 Fork 路径：v2 stages 实现
//!
//! Fork 语义：子 agent 继承父会话消息历史（parent_messages），用 prompt 继续执行。
//! Cancel policy = Cascade（父 cancel 传播到子）。无 agent_def（fork 是直接调用）。
//!
//! L3：创建（建 thread / session / 运行 / 收尾）统一经
//! `peri_agent::session::subagent::spawn_subagent`（Agent 层统一入口），
//! 本文件只组装意图（[`SubagentSpawnConfig`]）。

use std::sync::Arc;

use peri_agent::messages::BaseMessage;
use peri_agent::session::subagent::{
    extract_last_ai_text, format_subagent_result, ForkDirectiveKind, SubagentCancelPolicy,
    SubagentRunMode,
};
use peri_agent::tools::BaseTool;

impl super::SubAgentTool {
    pub(crate) async fn invoke_fork(
        &self,
        prompt: &str,
        cwd: &str,
        parent_messages: Vec<BaseMessage>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let host = self.host();

        // system prompt（frozen 优先 + system_builder 回退）
        let system_prompt = host
            .as_ref()
            .and_then(|h| h.frozen_system_prompt.clone())
            .map(|sp| sp.as_ref().to_string())
            .or_else(|| self.system_builder.as_ref().map(|b| b(None, cwd)));

        // LLM（fork 用默认 provider，无 model alias）；注入 event_handler，
        // 使 SubAgent 内 LLM 的重试事件能被父级 Langfuse 追踪
        let mut llm = (self.llm_factory)(None);
        llm.inject_event_handler(self.event_handler.clone());

        // 工具集：父工具 clone 为 Vec<Arc<dyn BaseTool>>
        let tools: Vec<Arc<dyn BaseTool>> = self.parent_tools.iter().cloned().collect();

        let config = self.spawn_config_base(
            "fork".to_string(),
            prompt.to_string(),
            parent_messages,
            SubagentCancelPolicy::Cascade,
            200,
            Some(ForkDirectiveKind::Fork),
            SubagentRunMode::Sync,
            llm,
            tools,
            system_prompt,
            Vec::new(),
            cwd.to_string(),
        );

        let spawned = self.spawn(config).await?;

        // Interrupted 语义与迁移前一致
        if spawned.interrupted {
            return Ok("Fork sub-agent execution was interrupted".to_string());
        }

        // 结果格式：thread_store 存在时带 child_thread_id 前缀（与迁移前一致）
        let text = extract_last_ai_text(&spawned.session);
        let output = peri_agent::agent::react::AgentOutput {
            text,
            steps: 0,
            tool_calls: Vec::new(),
            stop_reason: None,
            block_continue: None,
        };
        let result_text = format_subagent_result(&output);
        if host.as_ref().and_then(|h| h.thread_store.clone()).is_some() {
            Ok(format!(
                "child_thread_id: {}\n{}",
                spawned.child_thread_id, result_text
            ))
        } else {
            Ok(result_text)
        }
    }
}
