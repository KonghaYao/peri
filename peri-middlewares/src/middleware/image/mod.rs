mod compressor;

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
/// 在 `before_agent` 钩子中扫描最新一条 user message，查找 `@image <path>` 标记，
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

    async fn before_agent(
        &self,
        state: &mut dyn peri_agent::middleware::state::MiddlewareState,
    ) -> AgentResult<()> {
        // 取最后一条 Human 消息的索引
        let last_human_idx = state
            .messages()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, m)| matches!(m, BaseMessage::Human { .. }).then_some(i));

        let idx = match last_human_idx {
            Some(i) => i,
            None => return Ok(()),
        };

        let text = state.messages()[idx].content();
        let re = match Regex::new(r"@image\s+(\S+)") {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };

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
        let raw_results: Vec<Result<ImageFileData, String>> = tokio::task::spawn_blocking({
            let paths = paths.clone();
            move || {
                paths
                    .iter()
                    .map(|path| load_image_file(path, max_size))
                    .collect()
            }
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

        // 重建 MessageContent：删除 @image 文本，追加 Image/Error 块
        let clean_text = re.replace_all(&text, "").to_string();
        let clean_text = clean_text.trim().to_string();

        let mut new_blocks: Vec<ContentBlock> = Vec::new();
        if !clean_text.is_empty() {
            new_blocks.push(ContentBlock::text(clean_text));
        }

        for result in &results {
            match result {
                Ok(block) => new_blocks.push(block.clone()),
                Err(err) => new_blocks.push(ContentBlock::text(format!("[{}]", err))),
            }
        }

        let new_msg = state.messages()[idx].clone_with_content(MessageContent::Blocks(new_blocks));
        state.messages_mut()[idx] = new_msg;

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
