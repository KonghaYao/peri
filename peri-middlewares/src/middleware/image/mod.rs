mod compressor;

use peri_agent::middleware::capabilities as hook_state;
use std::path::Path;

use async_trait::async_trait;
use peri_agent::error::AgentResult;
use peri_agent::messages::{BaseMessage, ContentBlock, MessageContent};
use peri_agent::middleware::r#trait::Middleware;
use regex::Regex;

pub use compressor::{CompressorPipeline, ImageCompressor};

/// 图片支持的 MIME 类型
const SUPPORTED_MIME: &[(&str, &str)] = &[
    ("image/png", ".png"),
    ("image/jpeg", ".jpg"),
    ("image/gif", ".gif"),
    ("image/webp", ".webp"),
];

/// ImageMiddleware — 解析用户消息中的 @image <path>，替换为 ContentBlock::Image
///
/// 在 `before_agent` 钩子中扫描本批用户消息，查找 `@image <path>` 标记，
/// 读取对应图片文件，base64 编码后替换为 `ContentBlock::Image`。
/// 压缩管线为预留切面，MVP 为空——不对图片做任何压缩处理。
pub struct ImageMiddleware {
    max_size: usize,
    compressors: CompressorPipeline,
}

impl ImageMiddleware {
    pub fn new() -> Self {
        Self {
            max_size: 20 * 1024 * 1024, // 默认 20MB 上限
            compressors: CompressorPipeline::new(),
        }
    }

    /// 设置最大文件大小（字节）
    pub fn with_max_size(mut self, max_size: usize) -> Self {
        self.max_size = max_size;
        self
    }

    /// 添加压缩器
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

/// 文件加载结果：原始字节 + MIME 类型
struct ImageFileData {
    data: Vec<u8>,
    media_type: &'static str,
}

#[async_trait]
impl Middleware for ImageMiddleware {
    fn name(&self) -> &str {
        "ImageMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        let inputs: Vec<BaseMessage> = match state.input_message_ids() {
            Some(ids) => state
                .messages()
                .iter()
                .filter(|message| {
                    matches!(message, BaseMessage::Human { .. }) && ids.contains(&message.id())
                })
                .cloned()
                .collect(),
            None => state
                .messages()
                .iter()
                .rev()
                .find(|message| matches!(message, BaseMessage::Human { .. }))
                .cloned()
                .into_iter()
                .collect(),
        };
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
    async fn prepare_image_input(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
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

        // 在 blocking 线程中批量进行文件 I/O（读取 + MIME 检测）
        let max_size = self.max_size;
        let raw_results: Vec<Result<ImageFileData, String>> =
            tokio::task::spawn_blocking(move || {
                paths
                    .iter()
                    .map(|path| load_image_file(path, max_size))
                    .collect()
            })
            .await
            .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
                middleware: "ImageMiddleware".to_string(),
                reason: format!("spawn_blocking 失败: {e}"),
            })?;

        // 在主线程中执行压缩管线 + base64 编码
        let results: Vec<Result<ContentBlock, String>> = raw_results
            .into_iter()
            .map(|r| {
                r.map(|file_data| {
                    let processed = self.compressors.run(&file_data.data, file_data.media_type);
                    let base64_data = base64_encode(&processed);
                    ContentBlock::image_base64(file_data.media_type, base64_data)
                })
            })
            .collect();

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

/// 加载单张图片文件（仅在 blocking 线程中调用，执行文件 I/O + MIME 检测）
fn load_image_file(raw_path: &str, max_size: usize) -> Result<ImageFileData, String> {
    // 展开 ~ 和相对路径
    let expanded = shellexpand::tilde(raw_path).to_string();
    let path = Path::new(&expanded);

    if !path.exists() {
        return Err(format!("Image not found: {}", raw_path));
    }

    if !path.is_file() {
        return Err(format!("Not a file: {}", raw_path));
    }

    // 检查文件大小
    let metadata = std::fs::metadata(path).map_err(|e| format!("Cannot read file: {}", e))?;
    if metadata.len() > max_size as u64 {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        let max_mb = max_size as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "Image too large: {:.1}MB > {:.0}MB limit",
            size_mb, max_mb
        ));
    }

    // 读取文件
    let data = std::fs::read(path).map_err(|e| format!("Cannot read file: {}", e))?;

    // MIME 检测
    let media_type = detect_mime(&data).unwrap_or("application/octet-stream");
    if !SUPPORTED_MIME.iter().any(|(mime, _)| *mime == media_type) {
        return Err(format!("Not an image: {}", raw_path));
    }

    Ok(ImageFileData { data, media_type })
}

/// 使用 image crate 检测 MIME 类型
fn detect_mime(data: &[u8]) -> Option<&'static str> {
    use image::ImageFormat;
    let format = image::guess_format(data).ok()?;
    Some(match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        _ => return None,
    })
}

/// 标准 base64 编码
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
