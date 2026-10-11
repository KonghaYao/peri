//! 图片字节编码：RGBA → PNG（内存）。
//!
//! 仅供上传式附件使用——编码结果直接进 `PENDING_ATTACHMENTS`（base64），
//! 不落盘。写入走 [`LimitedWriter`]，超限立即失败，避免为会被拒收的图片
//! 分配整块编码缓冲。

/// 将 RGBA 字节数组编码为 PNG 字节，超过 `limit` 立即失败。
///
/// 错误形态：
/// - 尺寸/缓冲不一致、维度溢出 → `io::ErrorKind::InvalidInput`；
/// - 编码结果超过 `limit` → `io::ErrorKind::InvalidData`。
pub(crate) fn png_encode_bytes(
    rgba_bytes: &[u8],
    width: usize,
    height: usize,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    use std::io::{Error as IoError, ErrorKind};

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, "image dimensions overflow"))?;
    if rgba_bytes.len() != expected_len {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            format!(
                "RGBA buffer length {} does not match expected {}",
                rgba_bytes.len(),
                expected_len
            ),
        )
        .into());
    }
    let width = u32::try_from(width)
        .map_err(|_| IoError::new(ErrorKind::InvalidInput, "image width exceeds PNG limit"))?;
    let height = u32::try_from(height)
        .map_err(|_| IoError::new(ErrorKind::InvalidInput, "image height exceeds PNG limit"))?;

    let mut writer = LimitedWriter::new(limit);
    if let Err(error) = encode_png(&mut writer, rgba_bytes, width, height) {
        // 超限错误由 LimitedWriter 产生，但会经 png 编码器包装成 EncodingError——
        // 这里还原为原始 io::Error，保证调用方看到的上限语义稳定。
        return Err(match writer.limit_error() {
            Some(error) => error.into(),
            None => IoError::other(error).into(),
        });
    }
    Ok(writer.into_inner())
}

/// 流式写入 PNG 字节（不整体缓冲压缩结果）。
fn encode_png(
    writer: &mut LimitedWriter,
    rgba_bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<(), png::EncodingError> {
    use std::io::Write;

    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // Stream the compressed IDAT chunks. `write_image_data` first builds the
    // complete compressed image in memory, which defeats the ownership win
    // above for large clipboard images.
    let mut png_writer = encoder.write_header()?;
    {
        let mut stream = png_writer.stream_writer()?;
        stream.write_all(rgba_bytes)?;
        stream.finish()?;
    }
    // Writer::finish writes IEND and flushes its underlying writer. Keeping
    // this explicit ensures errors are propagated instead of being swallowed
    // by Drop.
    png_writer.finish()?;
    Ok(())
}

/// 只写内存的上限写入器：一旦累计字节超过 `limit` 立即报错。
struct LimitedWriter {
    buffer: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl LimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    /// 超限错误（未超限返回 `None`）。
    fn limit_error(&self) -> Option<std::io::Error> {
        self.exceeded.then(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encoded image exceeds {} byte limit", self.limit),
            )
        })
    }

    fn into_inner(self) -> Vec<u8> {
        self.buffer
    }
}

impl std::io::Write for LimitedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buffer.len() + data.len() > self.limit {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encoded image exceeds {} byte limit", self.limit),
            ));
        }
        self.buffer.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
