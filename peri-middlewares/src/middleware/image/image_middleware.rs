use super::reader;
use super::{
    async_trait, hook_state, AgentResult, Arc, BaseMessage, CompressorPipeline, ContentBlock,
    ImageCompressor, MessageContent, Middleware, Regex,
};
use std::fmt;
use std::fmt::{Debug, Formatter};

/// ImageMiddleware — 解析用户消息中的 @image <path>，替换为 ContentBlock::Image
///
/// 在 `before_input` 钩子中扫描本批用户消息，查找 `@image <path>` 标记，
/// 经 builtin Workspace MCP 读取图片，替换为 `ContentBlock::Image`；失败不回落本机文件系统。
/// 压缩管线为预留切面，MVP 为空——不对图片做任何压缩处理。
pub struct ImageMiddleware {
    pub(super) max_size: usize,
    pub(super) compressors: CompressorPipeline,
    pub(super) pool: Option<Arc<crate::mcp::McpClientPool>>,
    pub(super) session_id: Option<String>,
}

impl Debug for ImageMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageMiddleware")
            .field("max_size", &self.max_size)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl ImageMiddleware {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_size: 20 * 1024 * 1024, // 默认 20MB 上限
            compressors: CompressorPipeline::new(),
            pool: None,
            session_id: None,
        }
    }

    /// 设置最大文件大小（字节）
    #[must_use]
    pub fn with_max_size(mut self, max_size: usize) -> Self {
        self.max_size = max_size;
        self
    }

    #[must_use]
    pub fn with_mcp_pool(
        mut self,
        pool: Arc<crate::mcp::McpClientPool>,
        session_id: String,
        disabled: &std::collections::HashSet<String>,
    ) -> Self {
        let closed = crate::mcp::builtin::closed_instances(disabled);
        let source = pool.capability_source("workspace");
        self.pool =
            (!crate::mcp::builtin::is_capability_closed("workspace", source.as_ref(), &closed))
                .then_some(pool);
        self.session_id = Some(session_id);
        self
    }

    /// 添加压缩器
    #[must_use]
    pub fn with_compressor(mut self, compressor: Box<dyn ImageCompressor>) -> Self {
        self.compressors.add(compressor);
        self
    }
}

impl Default for ImageMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for ImageMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::Image.name()
    }

    async fn before_input(&self, state: &mut dyn hook_state::BeforeInputState) -> AgentResult<()> {
        // H7：只消费本批输入身份，不扫描历史最后一条 Human 充当新输入；空批次
        // （含没有批次身份的 legacy 适配器）不注入。
        let Some(ids) = state.input_message_ids() else {
            tracing::debug!(
                "ImageMiddleware: 本次输入没有批次身份（legacy 适配器），跳过附件转换且不扫描历史 Human"
            );
            return Ok(());
        };
        if ids.is_empty() {
            return Ok(());
        }
        let inputs: Vec<BaseMessage> = state
            .messages()
            .iter()
            .filter(|message| {
                matches!(message, BaseMessage::Human { .. }) && ids.contains(&message.id())
            })
            .cloned()
            .collect();
        let re = match Regex::new(r"@image\s+(\S+)") {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        for message in inputs {
            self.prepare_image_input(state, message, &re).await?;
        }
        Ok(())
    }
}

impl ImageMiddleware {
    pub(super) async fn prepare_image_input(
        &self,
        state: &mut dyn hook_state::BeforeInputState,
        message: BaseMessage,
        re: &Regex,
    ) -> AgentResult<()> {
        let text = message.content();
        // 收集所有 @image 路径
        let paths: Vec<String> = re
            .captures_iter(&text)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            .collect();

        if paths.is_empty() {
            return Ok(());
        }

        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let raw_result = reader::read_image(
                self.pool.as_deref(),
                self.session_id.as_deref(),
                &path,
                self.max_size,
            )
            .await;
            results.push(raw_result.map(|file_data| {
                let processed = self.compressors.run(&file_data.data, &file_data.media_type);
                let base64_data = base64_encode(processed.as_ref());
                ContentBlock::image_base64(file_data.media_type, base64_data)
            }));
        }

        // 只移除文本中的附件标记，保留输入原有的图片等内容块。
        let mut new_blocks: Vec<ContentBlock> = message
            .content_blocks()
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => {
                    let clean_text = re.replace_all(&text, "").trim().to_owned();
                    (!clean_text.is_empty()).then(|| ContentBlock::text(clean_text))
                }
                block => Some(block),
            })
            .collect();

        for result in results {
            match result {
                Ok(block) => new_blocks.push(block),
                Err(err) => new_blocks.push(ContentBlock::text(format!("[{}]", err))),
            }
        }

        let new_msg = message.clone_with_content(MessageContent::Blocks(new_blocks));
        if !state.replace_message(new_msg) {
            return Err(peri_agent::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: "image input message is no longer visible".to_string(),
            });
        }

        Ok(())
    }
}

/// 标准 base64 编码
pub(super) fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}
