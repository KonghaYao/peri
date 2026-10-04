mod parser;

use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use crate::workspace_io::WorkspaceFileReader;
use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::r#trait::Middleware,
};

const WORKSPACE_MENTION_REQUEST: &str = "workspace/readMention";

/// AtMentionMiddleware — 解析用户消息中的 @path 提及，注入 Workspace 读取结果
///
/// 在 `before_agent` 时从本批 Human 消息中提取 @ 提及，
/// 从会话 Workspace 读取对应内容，以 Ai[ToolUse{workspace/readMention}] →
/// Tool[ToolResult] 消息序列追加到 state。
///
/// 消息结构（与 SkillPreloadMiddleware 一致）：
/// ```text
/// [Human "用户消息（含 @path）"]
/// [Ai]    [ToolUse{workspace/readMention, call_{hex}}, ...]
/// [Tool]  ToolResult{call_{hex}, file_content}
/// ...
/// ```
pub struct AtMentionMiddleware {
    reader: Arc<dyn WorkspaceFileReader>,
}

impl AtMentionMiddleware {
    pub fn new(reader: Arc<dyn WorkspaceFileReader>) -> Self {
        Self { reader }
    }
}

#[async_trait]
impl Middleware for AtMentionMiddleware {
    fn name(&self) -> &str {
        "AtMentionMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        let inputs: Vec<String> = match state.input_message_ids() {
            Some(ids) => state
                .messages()
                .iter()
                .filter(|message| {
                    matches!(message, BaseMessage::Human { .. }) && ids.contains(&message.id())
                })
                .map(BaseMessage::content)
                .collect(),
            None => state
                .messages()
                .iter()
                .rev()
                .find(|message| matches!(message, BaseMessage::Human { .. }))
                .map(BaseMessage::content)
                .into_iter()
                .collect(),
        };
        for text in inputs {
            self.prepare_mentions(state, &text).await?;
        }
        Ok(())
    }
}

impl AtMentionMiddleware {
    async fn prepare_mentions(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
        text: &str,
    ) -> AgentResult<()> {
        let mentions = parser::extract_at_mentions(text);
        if mentions.is_empty() {
            return Ok(());
        }

        let mut file_contents = Vec::with_capacity(mentions.len());
        for mention in mentions {
            let result = self
                .reader
                .read_mention(&mention.path, mention.line_start, mention.line_end)
                .await;
            if let Err(error) = &result {
                tracing::debug!(path = %mention.path, %error, "workspace mention read skipped");
            }
            file_contents.push((mention, result.ok()));
        }

        // 过滤掉读取失败的
        let valid: Vec<_> = file_contents
            .into_iter()
            .filter_map(|(m, c)| c.map(|c| (m, c)))
            .collect();

        if valid.is_empty() {
            return Ok(());
        }

        // 生成 call_id
        let call_ids: Vec<String> = (0..valid.len())
            .map(|_| format!("call_{}", uuid::Uuid::new_v4().simple()))
            .collect();

        // 构造 ToolUse blocks
        let tool_use_blocks: Vec<ContentBlock> = valid
            .iter()
            .zip(call_ids.iter())
            .map(|((mention, _), id)| {
                let mut input = serde_json::json!({ "path": mention.path });
                if let Some(line_start) = mention.line_start {
                    input["lineStart"] = serde_json::json!(line_start);
                }
                if let Some(line_end) = mention.line_end {
                    input["lineEnd"] = serde_json::json!(line_end);
                }
                ContentBlock::tool_use(id.clone(), WORKSPACE_MENTION_REQUEST, input)
            })
            .collect();

        // 追加 Ai 消息
        state.add_message(BaseMessage::ai_from_blocks(tool_use_blocks));

        // 追加 ToolResult 消息
        for (id, (_mention, fc)) in call_ids.iter().zip(valid.iter()) {
            let prefix = match (fc.line_start, fc.line_end) {
                (Some(s), Some(e)) => format!("→ {} (L{s}-L{e})", fc.path),
                (Some(s), None) => format!("→ {} (L{s})", fc.path),
                _ => format!("→ {}", fc.path),
            };
            let content = format!("{prefix}\n{}", fc.content);
            state.add_message(BaseMessage::tool_result(id.clone(), content));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
