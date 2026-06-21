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

    const DESCRIPTION: &'static str = "长程目标跟踪工具。通过 action 参数区分操作：\n\
- create: 创建目标（objective 必填）。单个会话只能有一个 goal。\n\
- complete: 声明目标完成（经 LLM 验证，未通过会返回原因）\n\
- block: 声明遇到无法解决的阻塞（reason 必填）\n\
- get: 查询当前目标状态";

    async fn handle_create(
        objective: &str,
        controller: &dyn GoalController,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        controller
            .create_goal(objective.to_string())
            .await
            .map(|()| {
                format!(
                    "目标已创建: {objective}\n\n\
                     请围绕此目标持续推进。完成时调用 goal(complete)，\
                     遇到阻塞时调用 goal(block, reason)。"
                )
            })
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!("goal: 创建失败：{e}").into()
            })
    }

    async fn handle_complete(
        &self,
        ctx: &ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let snap = self.controller.snapshot();
        let objective = match &snap.objective {
            Some(o) => o.clone(),
            None => return Ok("无活跃 goal，无法 complete".to_string()),
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
                return Ok(format!("目标未达成: {}\n请继续工作。", verdict.missing));
            }
            // 验证通过：尝试转换状态。若期间状态已漂移到终态（如被 block），
            // 不作为 error 传播——LLM 验证已通过，agent 无需重试 complete
            match self.controller.complete_goal().await {
                Ok(()) => Ok(format!("目标已完成。验证证据: {}", verdict.evidence)),
                Err(e) => {
                    tracing::warn!(error = %e, "goal complete: 状态漂移到终态");
                    Ok(format!(
                        "目标已处于终态（{e}），无需再次 complete。验证证据: {}",
                        verdict.evidence
                    ))
                }
            }
        } else {
            // 无 auxiliary_model，跳过验证直接完成
            match self.controller.complete_goal().await {
                Ok(()) => Ok("目标已完成（跳过验证，未配置辅助 LLM）。".to_string()),
                Err(e) => {
                    tracing::warn!(error = %e, "goal complete: 状态漂移到终态");
                    Ok(format!("目标已处于终态（{e}），无需再次 complete。"))
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
        Ok(format!("目标已标记为阻塞: {reason}"))
    }

    fn handle_get(controller: &dyn GoalController) -> String {
        let snap = controller.snapshot();
        match (&snap.objective, snap.status) {
            (Some(obj), Some(status)) => {
                format!(
                    "目标: {obj}\n状态: {status}\n已用: {} tokens",
                    snap.tokens_used
                )
            }
            _ => "当前无目标。".to_string(),
        }
    }

    const VERIFY_SYSTEM_PROMPT: &'static str =
        "你是目标完成度评估器。判断 agent 是否达成了用户设定的目标。\n\
严格评估——只有确凿证据表明目标已达成才判 true。\n\n\
请输出 JSON 格式:\n\
{\"achieved\": true/false, \"evidence\": \"支撑判断的证据\", \"missing\": \"如未达成，还缺什么\"}";

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
            "目标: {objective}\n\n对话历史（最近 {} 条）:\n{}\n\n请判断目标是否已达成。",
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
                        .unwrap_or("未提供原因")
                        .to_string(),
                };
            }
        }
        // 解析失败，默认未达成
        GoalVerdict {
            achieved: false,
            evidence: String::new(),
            missing: "验证 LLM 输出解析失败".to_string(),
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
                    "description": "操作类型"
                },
                "objective": {
                    "type": "string",
                    "description": "create 时必填。目标描述，需具体可验证。"
                },
                "reason": {
                    "type": "string",
                    "description": "block 时必填。阻塞原因。"
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
