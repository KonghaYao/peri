//! GoalMiddleware — after_agent 钩子注入递增紧迫感 steering。
//!
//! goal active 时每轮注入提示 + 设 block_continue，executor 自动续跑。
//! agent 必须调 goal(complete) 或 goal(block) 才能终止循环。
//!
//! 注入路径：通过 v2 MessageQueue push Defer kind（Receive 保留 → End 消费唤醒续跑）。
//! 绝不破坏 frozen_system_prompt。使用 <system-reminder> 标签包裹，
//! 与 compact 摘要检测、14_system_reminder prompt 协同。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, MessageContent},
    middleware::{r#trait::Middleware, state::MiddlewareState},
    session::{MessageKind, MessageSource, QueuedMessage},
};

use crate::goal::GoalTool;

/// Goal steering 注入中间件（链最后）
pub struct GoalMiddleware {
    controller: Arc<dyn peri_agent::goal::GoalController>,
    /// 辅助 LLM（complete 验证用），None 时跳过验证
    auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    /// 连续 after_agent 注入次数（递增紧迫感用）
    pending_rounds: AtomicUsize,
}

impl GoalMiddleware {
    pub fn new(
        controller: Arc<dyn peri_agent::goal::GoalController>,
        auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    ) -> Self {
        Self {
            controller,
            auxiliary_model,
            pending_rounds: AtomicUsize::new(0),
        }
    }

    /// 渲染递增紧迫感模板
    fn render_steering(objective: &str, round: usize) -> String {
        let urgency = match round {
            1 => {
                "You gave a response without declaring the goal complete. Decide:\n\
                  - Achieved → goal(complete)\n\
                  - Blocked → goal(block, reason)\n\
                  - Need to continue → proceed with the next step"
            }
            2 => {
                "The goal is not yet complete. You must call goal(complete) or goal(block) to end, or continue with the next step."
            }
            _ => {
                "Attention: the goal is still not complete. Decide immediately — keep working or declare a terminal state."
            }
        };
        format!(
            "<goal-message>\n\
             [Goal Steering]\n\
             Objective: {objective}\n\
             {urgency}\n\
             </goal-message>"
        )
    }
}

#[async_trait]
impl Middleware for GoalMiddleware {
    fn name(&self) -> &str {
        "GoalMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn peri_agent::tools::BaseTool>> {
        // Goal 工具通过 collect_tools 注册到 shared_tools（executor 每轮 clear + repopulate）
        // is_direct() 默认为 false，该工具不会直接出现在 LLM 工具列表中，仅通过 SearchExtraTools → ExecuteExtraTool 访问
        vec![Box::new(GoalTool::new(
            Arc::clone(&self.controller),
            self.auxiliary_model.clone(),
        ))]
    }

    async fn after_agent(
        &self,
        state: &mut dyn MiddlewareState,
        output: &peri_agent::agent::react::AgentOutput,
    ) -> AgentResult<peri_agent::agent::react::AgentOutput> {
        // 1. 前面已有 block_continue（如 HookMiddleware stop block）→ 不干预
        if output.block_continue.is_some() {
            return Ok(output.clone());
        }

        // 2. 检查 goal 状态
        let snap = self.controller.snapshot();
        if !peri_agent::goal::is_goal_active(&snap) {
            // 无 goal 或终态 → 重置计数器 + 放行
            self.pending_rounds.store(0, Ordering::Relaxed);
            return Ok(output.clone());
        }

        // 3. goal active → 注入递增紧迫感 steering
        let round = self.pending_rounds.fetch_add(1, Ordering::Relaxed) + 1;
        let objective = snap.objective.as_deref().unwrap_or("(unknown)");
        let template = Self::render_steering(objective, round);
        // [TRAP] 必须用 Human + <system-reminder> 注入，禁止 BaseMessage::system。
        // System 消息会被 invoke hoist 到 system prompt 顶部，污染 frozen_system_prompt。
        // （与 hooks/middleware.rs stop_hook_feedback、compact_v2.rs::re_inject_v2 注入路径一致）
        // 走 v2 MessageQueue Defer kind → Receive 阶段 drain_all 消费。
        let reminder = format!("<system-reminder>\n{}\n</system-reminder>", template);
        state.v2_queue().push(QueuedMessage::new(
            MessageKind::Defer,
            MessageSource::GoalSteering,
            BaseMessage::human(MessageContent::text(reminder)),
        ));

        tracing::debug!(
            objective = %objective,
            round = round,
            "GoalMiddleware: 注入 after_agent steering"
        );

        // 4. 设 block_continue，executor 自动续跑
        let mut output = output.clone();
        output.block_continue = Some("goal_active".to_string());
        Ok(output)
    }
}

#[cfg(test)]
#[path = "goal_middleware_test.rs"]
mod tests;
