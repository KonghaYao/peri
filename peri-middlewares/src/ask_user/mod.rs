// 从核心库导入 trait 和数据类型
pub use peri_agent::interaction::{InteractionContext, QuestionItem, QuestionOption};
use peri_agent::{agent::react::ToolCall, error::AgentError};

use crate::tool_search::core_tools::TOOL_ASK_USER;

// ─── 解析辅助 ──────────────────────────────────────────────────────────────────

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

/// 将一个 ToolCall 解析为 QuestionItem 列表；非 ask_user_question 工具返回空 Vec。
pub fn parse_ask_user(tool_call: &ToolCall) -> Result<Vec<QuestionItem>, AgentError> {
    if tool_call.name != TOOL_ASK_USER {
        return Ok(vec![]);
    }
    let input: AskUserInput = serde_json::from_value(tool_call.input.clone()).map_err(|e| {
        AgentError::ToolExecutionFailed {
            tool: TOOL_ASK_USER.to_string(),
            reason: format!("参数解析失败: {e}"),
        }
    })?;
    Ok(input
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

// ─── `ask_user_question` 工具定义 ─────────────────────────────────────────────

/// `ask_user_question` tool definition (aligned with Claude AskUserQuestion)
pub fn ask_user_tool_definition() -> peri_agent::tools::ToolDefinition {
    peri_agent::tools::ToolDefinition {
        name: TOOL_ASK_USER.to_string(),
        description: "Batch ask users questions with options to get their selection or custom input.\
                      Use when a task requires users to provide details, preferences, or make choices.\
                      One call supports 1-4 questions, all displayed together to the user.\
                      Each question provides a clear list of options, and users can always input custom content."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "description": "List of questions, 1-4 items",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The question to ask the user, clear and specific with necessary context"
                            },
                            "header": {
                                "type": "string",
                                "description": "Short header (<=12 characters) for UI Tab display, e.g.: color preference, deployment method"
                            },
                            "multiSelect": {
                                "type": "boolean",
                                "default": false,
                                "description": "Whether to allow multiple selections, defaults to false (single select)"
                            },
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "Option display text, concise and clear (1-50 characters)"
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Option explanation, explaining the option's meaning or applicable scenario (optional)"
                                        },
                                        "preview": {
                                            "type": "string",
                                            "description": "Preview content, showing the effect or example of the option (optional)"
                                        }
                                    },
                                    "required": ["label"]
                                },
                                "minItems": 2,
                                "maxItems": 4,
                                "description": "Option list, at least 2 items, at most 4 items"
                            }
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        }),
    }
}
