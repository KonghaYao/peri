//! Goal 工具 — 单一 deferred 工具，通过 action 参数分发。
//!
//! action: create / complete / block / get
//! complete 经 auxiliary_model LLM 二元验证。

use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::goal::GoalController;
use peri_agent::llm::{BaseModel, LlmRequest};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::{BaseTool, ToolContext};
use serde_json::{json, Value};

/// Goal 工具
pub struct GoalTool {
    controller: Arc<dyn GoalController>,
    /// 辅助 LLM（complete 验证用），None 时跳过验证
    auxiliary_model: Option<Arc<dyn BaseModel>>,
}

impl GoalTool {
    pub fn new(
        controller: Arc<dyn GoalController>,
        auxiliary_model: Option<Arc<dyn BaseModel>>,
    ) -> Self {
        Self {
            controller,
            auxiliary_model,
        }
    }

    const DESCRIPTION: &'static str =
        "Long-running goal tracking tool. Use the action parameter to select an operation:\n\
- create: Create a goal (objective required). Only one goal per session.\n\
- complete: Declare the goal complete (verified by an auxiliary LLM; returns reason if not met)\n\
- block: Declare an unsolvable blocker (reason required)\n\
- get: Query current goal status";

    async fn handle_create(
        objective: &str,
        controller: &dyn GoalController,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        controller
            .create_goal(objective.to_string())
            .await
            .map(|()| {
                format!(
                    "Goal created: {objective}\n\n\
                     Keep working toward this goal. Call goal(complete) when done, \
                     or goal(block, reason) if blocked."
                )
            })
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!("goal: failed to create: {e}").into()
            })
    }

    async fn handle_complete(
        &self,
        ctx: &ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let snap = self.controller.snapshot();
        let objective = match &snap.objective {
            Some(o) => o.clone(),
            None => return Ok("No active goal to complete".to_string()),
        };

        // auxiliary_model 为 None 时跳过验证
        if let Some(model) = &self.auxiliary_model {
            let user_content = Self::build_verify_prompt(&objective, ctx.messages);
            let request = LlmRequest::new(vec![BaseMessage::human(user_content)])
                .with_system(Self::VERIFY_SYSTEM_PROMPT.to_string())
                .with_max_tokens(1024);

            let response = model.invoke(request).await?;
            let raw = response.message.content();

            let verdict = Self::parse_verdict(&raw);
            if !verdict.achieved {
                // 验证失败：goal 保持 Active
                return Ok(format!(
                    "Goal not yet achieved: {}\nKeep working.",
                    verdict.missing
                ));
            }
            // 验证通过：尝试转换状态。若期间状态已漂移到终态（如被 block），
            // 不作为 error 传播——LLM 验证已通过，agent 无需重试 complete
            match self.controller.complete_goal().await {
                Ok(()) => Ok(format!(
                    "Goal completed. Verification evidence: {}",
                    verdict.evidence
                )),
                Err(e) => {
                    tracing::warn!(error = %e, "goal complete: 状态漂移到终态");
                    Ok(format!(
                        "Goal is already in a terminal state ({e}). Verification evidence: {}",
                        verdict.evidence
                    ))
                }
            }
        } else {
            // 无 auxiliary_model，跳过验证直接完成
            match self.controller.complete_goal().await {
                Ok(()) => Ok(
                    "Goal completed (verification skipped, no auxiliary LLM configured)."
                        .to_string(),
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "goal complete: 状态漂移到终态");
                    Ok(format!("Goal is already in a terminal state ({e})."))
                }
            }
        }
    }

    async fn handle_block(
        &self,
        reason: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.controller
            .block_goal(reason.to_string())
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
        Ok(format!("Goal marked as blocked: {reason}"))
    }

    fn handle_get(controller: &dyn GoalController) -> String {
        let snap = controller.snapshot();
        match (&snap.objective, snap.status) {
            (Some(obj), Some(status)) => {
                format!(
                    "Objective: {obj}\nStatus: {status}\nTokens used: {}",
                    snap.tokens_used
                )
            }
            _ => "No active goal.".to_string(),
        }
    }

    const VERIFY_SYSTEM_PROMPT: &'static str =
        "You are a goal completion evaluator. Determine whether the agent has achieved the user's goal.\n\
        Be strict — only return true if there is concrete evidence the goal was met.\n\n\
        Output JSON in this format:\n\
        {\"achieved\": true/false, \"evidence\": \"evidence supporting the judgment\", \"missing\": \"if not achieved, what is still missing\"}";

    fn role_label(msg: &BaseMessage) -> &'static str {
        match msg {
            BaseMessage::Human { .. } => "user",
            BaseMessage::Ai { .. } => "assistant",
            BaseMessage::System { .. } => "system",
            BaseMessage::Tool { .. } => "tool",
        }
    }

    /// 验证 prompt 中保留的最近消息数（避免 auxiliary_model 上下文窗口溢出）
    const VERIFY_RECENT_MESSAGES: usize = 20;

    fn build_verify_prompt(objective: &str, messages: &[BaseMessage]) -> String {
        // 过滤 System 消息（frozen system prompt 无助于判断目标完成度，且可能很长）
        let filtered: Vec<&BaseMessage> = messages
            .iter()
            .filter(|m| !matches!(m, BaseMessage::System { .. }))
            .collect();
        // 取最近 N 条，避免长会话下 auxiliary_model 的上下文窗口溢出
        let recent: &[&BaseMessage] = if filtered.len() > Self::VERIFY_RECENT_MESSAGES {
            &filtered[filtered.len() - Self::VERIFY_RECENT_MESSAGES..]
        } else {
            &filtered[..]
        };
        let transcript: Vec<String> = recent
            .iter()
            .map(|m| format!("[{}] {}", Self::role_label(m), m.content()))
            .collect();
        format!(
            "Objective: {objective}\n\nConversation history (most recent {} messages):\n{}\n\nDetermine whether the objective has been achieved.",
            recent.len(),
            transcript.join("\n")
        )
    }

    fn parse_verdict(raw: &str) -> GoalVerdict {
        // 宽松解析：找第一个 { 到最后一个 }
        let start = raw.find('{');
        let end = raw.rfind('}');
        if let (Some(s), Some(e)) = (start, end) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw[s..=e]) {
                return GoalVerdict {
                    achieved: v.get("achieved").and_then(|a| a.as_bool()).unwrap_or(false),
                    evidence: v
                        .get("evidence")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string(),
                    missing: v
                        .get("missing")
                        .and_then(|m| m.as_str())
                        .unwrap_or("no reason provided")
                        .to_string(),
                };
            }
        }
        // 解析失败，默认未达成
        GoalVerdict {
            achieved: false,
            evidence: String::new(),
            missing: "Failed to parse verifier LLM output".to_string(),
        }
    }
}

struct GoalVerdict {
    achieved: bool,
    evidence: String,
    missing: String,
}

#[async_trait]
impl BaseTool for GoalTool {
    fn name(&self) -> &str {
        "goal"
    }

    fn description(&self) -> &str {
        Self::DESCRIPTION
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "complete", "block", "get"],
                    "description": "Operation type"
                },
                "objective": {
                    "type": "string",
                    "description": "Required for create. The goal description — must be specific and verifiable."
                },
                "reason": {
                    "type": "string",
                    "description": "Required for block. The reason the goal cannot be completed."
                }
            },
            "required": ["action"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or("goal: missing required 'action' parameter")?;

        match action {
            "create" => {
                let objective = input
                    .get("objective")
                    .and_then(|v| v.as_str())
                    .ok_or("goal: create requires 'objective' parameter")?;
                Self::handle_create(objective, self.controller.as_ref()).await
            }
            "complete" => self.handle_complete(&ctx).await,
            "block" => {
                let reason = input
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .ok_or("goal: block requires 'reason' parameter")?;
                self.handle_block(reason).await
            }
            "get" => Ok(Self::handle_get(self.controller.as_ref())),
            other => Err(format!("goal: unknown action '{other}'").into()),
        }
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
