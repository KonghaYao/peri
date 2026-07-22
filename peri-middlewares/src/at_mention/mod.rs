mod file_reader;
mod parser;

use std::path::PathBuf;

use async_trait::async_trait;
pub use file_reader::FileContent;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock},
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

use crate::tool_search::core_tools::TOOL_READ;

/// AtMentionMiddleware — 解析用户消息中的 @path 提及，注入 Read 工具调用结果
///
/// 在 `before_agent` 时从最后一条 Human 消息中提取 @ 提及，
/// 读取对应文件内容，以 Ai[ToolUse{Read}] → Tool[ToolResult] 消息序列追加到 state。
///
/// 消息结构（与 SkillPreloadMiddleware 一致）：
/// ```text
/// [Human "用户消息（含 @path）"]
/// [Ai]    [ToolUse{Read, call_{hex}}, ...]
/// [Tool]  ToolResult{call_{hex}, file_content}
/// ...
/// ```
pub struct AtMentionMiddleware {
    cwd: PathBuf,
}

impl AtMentionMiddleware {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Middleware for AtMentionMiddleware {
    fn name(&self) -> &str {
        "AtMentionMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 取最后一条 Human 消息
        let last_human = state
            .messages()
            .iter()
            .rev()
            .find(|m| matches!(m, BaseMessage::Human { .. }));

        let text = match last_human {
            Some(msg) => msg.content(),
            None => return Ok(()),
        };

        let mentions = parser::extract_at_mentions(&text);
        if mentions.is_empty() {
            return Ok(());
        }

        // 在 blocking 线程中读取文件
        let cwd = self.cwd.clone();
        let file_contents: Vec<(parser::AtMention, Option<FileContent>)> =
            tokio::task::spawn_blocking(move || {
                mentions
                    .into_iter()
                    .map(|m| {
                        let content =
                            file_reader::read_file_content(&cwd, &m.path, m.line_start, m.line_end);
                        (m, content)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "AtMentionMiddleware".to_string(),
                reason: format!("spawn_blocking 失败: {e}"),
            })?;

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
                let mut input = serde_json::json!({
                    "file_path": mention.path,
                });
                if let Some(offset) = mention.line_start {
                    input["offset"] = serde_json::json!(offset);
                }
                ContentBlock::tool_use(id.clone(), TOOL_READ, input)
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
