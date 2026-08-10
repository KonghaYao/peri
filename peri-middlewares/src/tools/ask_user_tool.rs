use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    interaction::{
        InteractionContext, InteractionResponse, QuestionItem, QuestionOption,
        UserInteractionBroker,
    },
    tools::BaseTool,
};
use serde_json::Value;

use crate::ask_user::ask_user_tool_definition;

// ─── AskUserTool ──────────────────────────────────────────────────────────────

/// `ask_user_question` 工具的 BaseTool 实现
///
/// 将 ask_user_question LLM 工具调用转化为对 [`UserInteractionBroker`] 的调用，
/// 挂起等待用户通过 UI 提供答案后恢复。支持单次调用传入 1–4 个问题。
pub struct AskUserTool {
    broker: Arc<dyn UserInteractionBroker>,
}

impl AskUserTool {
    pub fn new(broker: Arc<dyn UserInteractionBroker>) -> Self {
        Self { broker }
    }
}

// ─── 解析辅助 ─────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct InputOption {
    label: String,
    description: Option<String>,
    _preview: Option<String>,
}

#[derive(serde::Deserialize)]
struct InputQuestion {
    question: String,
    header: String,
    #[serde(default, rename = "multiSelect")]
    multi_select: bool,
    options: Vec<InputOption>,
}

#[derive(serde::Deserialize)]
struct AskUserInput {
    questions: Vec<InputQuestion>,
}

fn parse_questions(
    input: Value,
) -> Result<Vec<QuestionItem>, Box<dyn std::error::Error + Send + Sync>> {
    let parsed: AskUserInput = serde_json::from_value(input)
        .map_err(|e| format!("ask_user_question: 参数解析失败: {e}"))?;
    Ok(parsed
        .questions
        .into_iter()
        .enumerate()
        .map(|(i, q)| QuestionItem {
            id: format!("ask_user_question_{i}"),
            question: q.question,
            header: q.header,
            options: q
                .options
                .into_iter()
                .map(|o| QuestionOption {
                    label: o.label,
                    description: o.description,
                })
                .collect(),
            multi_select: q.multi_select,
        })
        .collect())
}

#[async_trait]
impl BaseTool for AskUserTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 提示词层声明分组（design v2 §2.5.1）：交互类工具归入 `interaction`。
    fn namespace(&self) -> Option<&str> {
        Some("interaction")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：向用户提问，含批量合并纪律。
    ///
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    /// 05_using_tools.md 手写条目在渐进迁移完成前保留（守护测试防逐字重复）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Ask the user to decide → `{{name}}` ({{title}}): structured choices, 1 to 4 questions per call. Group every independent question into one call (3 questions in one call beats 3 separate calls; drop to 1 only when nothing else needs clarifying), and prefer `{{name}}` to free-text hedging or guessing."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        ask_user_tool_definition().description.leak()
    }

    fn parameters(&self) -> Value {
        ask_user_tool_definition().parameters
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let questions = parse_questions(input)?;
        let headers: Vec<String> = questions.iter().map(|q| q.header.clone()).collect();

        let ctx = InteractionContext::Questions {
            requests: questions,
        };
        let response = self.broker.request(ctx).await;

        match response {
            InteractionResponse::Answers(answers) => {
                let parts: Vec<String> = headers
                    .iter()
                    .zip(answers.iter())
                    .map(|(header, answer)| {
                        let val = if let Some(ref text) =
                            answer.text.as_ref().filter(|t| !t.is_empty())
                        {
                            text.to_string()
                        } else {
                            answer.selected.join(", ")
                        };
                        format!("[问: {header}]\n回答: {val}")
                    })
                    .collect();
                Ok(parts.join("\n\n"))
            }
            InteractionResponse::Rejected => {
                Err(Box::new(peri_agent::error::AgentError::ToolRejected {
                    tool: "AskUserQuestion".to_string(),
                    reason: "用户拒绝回答".to_string(),
                }))
            }
            _ => Err("ask_user_question: unexpected response type".into()),
        }
    }
}

#[cfg(test)]
#[path = "ask_user_tool_test.rs"]
mod tests;
