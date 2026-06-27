//! 流式渲染模式及 Block 模式缓冲区管理。

/// 流式渲染模式：控制 LLM 输出时的渲染粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StreamingMode {
    /// 逐 token 实时渲染 + 自适应帧率（默认）
    #[default]
    Streaming,
    /// 按 Markdown block 粒度整块渲染（段落/代码块完成后渲染）
    Block,
    /// 不渲染流式内容，LLM 完成后一次性显示
    None,
}

impl super::MessagePipeline {
    /// 追加流式文本 chunk
    pub(crate) fn push_chunk(&mut self, chunk: &str) {
        match self.streaming_mode {
            StreamingMode::Streaming => {
                let partial = self.partial_mut();
                partial.text.push_str(chunk);
                self.adaptive_policy.on_chunk(chunk);
            }
            StreamingMode::Block => {
                if self.push_chunk_block(chunk) {
                    self.flush_block_buffer();
                }
            }
            StreamingMode::None => {
                let partial = self.partial_mut();
                partial.text.push_str(chunk);
            }
        }
    }

    /// 追加推理 chunk
    pub(crate) fn push_reasoning(&mut self, text: &str) {
        let partial = self.partial_mut();
        partial.reasoning.push_str(text);
        self.adaptive_policy.on_reasoning_chunk();
    }

    // ─── Block 模式缓冲区管理 ────────────────────────────────────────────

    /// Block 模式下追加 chunk 到缓冲区并检测 block 边界。返回 true 表示检测到边界。
    fn push_chunk_block(&mut self, chunk: &str) -> bool {
        self.block_buffer.push_str(chunk);

        if self.inside_code_fence {
            if self.detect_code_fence_close() {
                self.inside_code_fence = false;
                return true;
            }
        } else {
            if self.block_buffer.contains("\n\n") {
                return true;
            }
            if self.detect_code_fence_open() {
                self.inside_code_fence = true;
            }
        }
        false
    }

    fn detect_code_fence_open(&self) -> bool {
        self.block_buffer
            .lines()
            .last()
            .is_some_and(|line| line.trim_start().starts_with("```"))
    }

    fn detect_code_fence_close(&self) -> bool {
        self.block_buffer
            .lines()
            .last()
            .is_some_and(|line| line.trim() == "```")
    }

    fn flush_block_buffer(&mut self) {
        if !self.block_buffer.is_empty() {
            // 先 take 出 buffer 内容，避免 &mut self.partial 与 &self.block_buffer 借用冲突
            let buffered = std::mem::take(&mut self.block_buffer);
            let partial = self.partial_mut();
            partial.text.push_str(&buffered);
            self.block_pending_flush = true;
        }
    }

    pub(crate) fn force_flush_block(&mut self) {
        self.flush_block_buffer();
        self.inside_code_fence = false;
    }

    /// 检查 Block 模式是否有待 flush 的内容
    pub(crate) fn has_pending_block_flush(&self) -> bool {
        self.block_pending_flush || !self.block_buffer.is_empty()
    }

    /// 获取当前流式渲染模式
    pub(crate) fn streaming_mode(&self) -> StreamingMode {
        self.streaming_mode
    }

    /// 设置流式渲染模式。切换时强制 flush Block 缓冲区。
    pub(crate) fn set_streaming_mode(&mut self, mode: StreamingMode) {
        if self.streaming_mode == StreamingMode::Block && mode != StreamingMode::Block {
            self.flush_block_buffer();
        }
        self.streaming_mode = mode;
        self.inside_code_fence = false;
        tracing::info!(?mode, "streaming mode changed");
    }

    /// 从配置字符串设置初始模式（"streaming" / "block" / "none"）
    pub fn init_streaming_mode_from_config(&mut self, mode_str: &str) {
        let mode = match mode_str {
            "block" => StreamingMode::Block,
            "none" => StreamingMode::None,
            _ => StreamingMode::Streaming,
        };
        self.streaming_mode = mode;
    }
}
