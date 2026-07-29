//! 图片压缩切面 —— 为后续实现预留接口，MVP 空管线。

use std::error::Error;

/// 图片压缩器 trait
///
/// 每个压缩器实现一种压缩策略（尺寸缩放、JPEG 质量、PNG 量化等）。
/// 管线上各压缩器按注册顺序依次执行。
pub trait ImageCompressor: Send + Sync {
    /// 压缩器名称（用于日志/调试）
    fn name(&self) -> &str;

    /// 对图片字节进行压缩
    ///
    /// # 参数
    /// - `data`: 原始图片字节
    /// - `media_type`: MIME 类型（如 "image/png"）
    ///
    /// # 返回
    /// - `Ok(compressed_bytes)`: 压缩后的字节
    /// - `Err(_)`: 压缩失败（此时应降级使用原始数据）
    fn compress(
        &self,
        data: &[u8],
        media_type: &str,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
}

/// 压缩管线 —— 依次执行多个压缩器
pub struct CompressorPipeline {
    compressors: Vec<Box<dyn ImageCompressor>>,
}

impl CompressorPipeline {
    /// 创建空管线（MVP 默认无压缩器）
    pub fn new() -> Self {
        Self {
            compressors: Vec::new(),
        }
    }

    /// 添加压缩器
    pub fn add(&mut self, compressor: Box<dyn ImageCompressor>) {
        self.compressors.push(compressor);
    }

    /// 按序执行压缩链，任一失败则降级返回原始数据
    pub fn run(&self, data: &[u8], media_type: &str) -> Vec<u8> {
        let mut current = data.to_vec();
        for c in &self.compressors {
            match c.compress(&current, media_type) {
                Ok(compressed) => current = compressed,
                Err(_) => return data.to_vec(), // 降级：返回原始数据
            }
        }
        current
    }

    /// 管线是否为空
    pub fn is_empty(&self) -> bool {
        self.compressors.is_empty()
    }
}

impl Default for CompressorPipeline {
    fn default() -> Self {
        Self::new()
    }
}
