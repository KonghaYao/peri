//! SubAgent 后台路径：v2 stages 实现
//!
//! Agent 调用恒为后台（v5 行为）：新建、fork、非 active resume 都经
//! `SessionFactory` 启动后台执行，主流程立即返回；同步执行实现保留在 Agent 层
//! （`SubagentRunMode::Sync` / `run_sync.rs`），但本工具不再产生该路径。
//!
//! 1. 经 `build_agent_from_def` 装配 v2 字段（cancel_policy=Independent）
//! 2. 组装 [`SubagentSpawnConfig`](peri_agent::session::subagent::SubagentSpawnConfig) 经 [`SessionFactory::spawn_subagent`](peri_agent::session::subagent::SessionFactory::spawn_subagent)（Agent 层统一入口）：
//!    tokio::spawn 内运行 `run_react_loop`，主流程立即返回
//! 3. 任务完成时统一入口负责 bg_event_sender 通知主 agent + lifecycle hook +
//!    thread_store 更新 + TaskManager 收尾
//!
//! L3：本文件只组装意图，不持有创建实现。

use std::sync::Arc;

use peri_agent::messages::BaseMessage;
use peri_agent::session::subagent::{
    ForkDirectiveKind, SubagentCancelPolicy, SubagentRunMode, SubagentSpawned,
};
use peri_agent::tools::BaseTool;
use peri_mcp_core::agent_definition::ClaudeAgent;

impl super::SubAgentTool {
    /// 后台执行的前置能力门：v5 起所有 Agent 调用都要求任务管理器。
    ///
    /// 无管理器时显式报错，不静默落回同步（同步路径已不可唤醒）。
    fn require_background_support(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.host().task_manager.is_none() {
            return Err(
                "Error: Agent execution requires a background task manager; none is configured"
                    .into(),
            );
        }
        Ok(())
    }

    /// fork 的后台启动：父消息注入 + fork directive 包装；model 参数忽略。
    pub(crate) async fn invoke_background_fork(
        &self,
        prompt: String,
        cwd: String,
        parent_messages: Vec<BaseMessage>,
        parent_tool_call_id: Option<String>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.require_background_support()?;
        let llm = (self.llm_factory)(None);
        // 子身份 system（H2 子能力投影）：fork 无定义 overrides，由
        // system_builder 重建；不复制父冻结字节。
        let system_prompt = self.system_builder.as_ref().map(|b| b(None, &cwd));
        let tools: Vec<Arc<dyn BaseTool>> = self.parent_tools.iter().cloned().collect();
        let config = self.spawn_config_base(
            "fork".to_string(),
            prompt,
            parent_messages,
            SubagentCancelPolicy::Independent,
            crate::subagent::DEFAULT_SUBAGENT_MAX_ITERATIONS,
            Some(ForkDirectiveKind::Fork),
            SubagentRunMode::Background,
            llm,
            tools,
            crate::subagent::fork::child_inheritance_filter(),
            system_prompt,
            Vec::new(),
            cwd,
            parent_tool_call_id,
        );
        let spawned = self.spawn(config).await?;
        Ok(self.background_receipt(&spawned))
    }

    /// 定义型 agent 的后台启动。
    ///
    /// 定义由调用方先解析（MCP Agent 已经在审批门批准），本函数不再加载定义：
    /// 后台路径不得绕过 MCP Agent 的内容绑定审批。
    #[allow(clippy::too_many_arguments)] // 通道参数逐项显式传递，与 spawn_config_base 同口径
    pub(crate) async fn invoke_background_agent(
        &self,
        agent_id: String,
        agent_def: ClaudeAgent,
        prompt: String,
        cwd: String,
        model: Option<&str>,
        parent_tool_call_id: Option<String>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.require_background_support()?;
        let build_result = self
            .build_agent_from_def(&agent_def, &agent_id, &cwd, model)
            .await?;
        let llm = build_result.llm;
        let config = self.spawn_config_base(
            agent_id,
            prompt,
            Vec::new(),
            SubagentCancelPolicy::Independent,
            build_result.max_iterations,
            None,
            SubagentRunMode::Background,
            llm,
            build_result
                .tools
                .into_iter()
                .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
                .collect(),
            build_result.tool_filter,
            build_result.system_prompt,
            build_result.skill_names,
            cwd,
            parent_tool_call_id,
        );
        let spawned = self.spawn(config).await?;
        Ok(self.background_receipt(&spawned))
    }

    /// 后台启动回执。线程身份是 resume 的唯一可用身份（task id 不是）。
    fn background_receipt(&self, spawned: &SubagentSpawned) -> String {
        let task_id = spawned
            .task_id
            .clone()
            .unwrap_or_else(|| "bg-unknown".to_string());
        if self.host().session_resources.is_some() {
            // 回执直接给出可复用的线程身份：规范句与 bg 完成通知（peri-acp-types
            // event.rs）逐字同形，模型无需从两个裸 UUID 里推测哪个能 resume；
            // task_id 保留（取消/状态查询用），但不带 `(thread: …)` 这类无指引标注
            // ——task 身份（`bg-{uuid}`）与 child thread 身份是两次独立生成的 v7 UUID，
            // 前缀相同，去掉 `bg-` 仍不是线程身份（resume 会以 thread not found 拒绝）。
            format!(
                "Background task {} started. You will be notified when it completes. \
                 You can continue with other tasks in the meantime.\n\
                 child_thread_id: {} (resume with Agent(resume_thread_id: {}))\n\
                 While active, include a non-empty prompt to send supplemental instructions; \
                 omit prompt only when resuming a non-active thread.",
                task_id, spawned.child_thread_id, spawned.child_thread_id
            )
        } else {
            format!(
                "Background task {} started. You will be notified when it completes. \
                 You can continue with other tasks in the meantime.",
                task_id
            )
        }
    }
}
