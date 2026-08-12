use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::LspError;

/// Content-Length 分帧编码
///
/// 格式: `"Content-Length: {length}\r\n\r\n{body}"`
pub async fn encode_message(
    msg: &[u8],
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<(), LspError> {
    let header = format!("Content-Length: {}\r\n\r\n", msg.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(msg).await?;
    writer.flush().await?;
    Ok(())
}

/// 单帧 body 大小上限：防异常/恶意 Content-Length 声明触发大分配
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Content-Length 分帧解码
///
/// 读取 `"Content-Length: {N}\r\n\r\n"` 头部行，然后读取 N 字节 body。
/// 头部字段名大小写不敏感（RFC 7230）：小写 `content-length:` 不应丢帧。
/// body 声明超过 `MAX_MESSAGE_BYTES` 时直接报错，不做对应大小的分配。
/// 返回 None 表示 EOF。
pub async fn decode_message(
    reader: &mut (impl AsyncBufReadExt + Unpin),
) -> Result<Option<String>, LspError> {
    // 读取头部行
    let mut header_line = String::new();
    loop {
        header_line.clear();
        let bytes_read = reader.read_line(&mut header_line).await?;
        if bytes_read == 0 {
            return Ok(None); // EOF
        }
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(content_length_str) =
            trimmed.to_ascii_lowercase().strip_prefix("content-length:")
        {
            let content_length: usize =
                content_length_str
                    .trim()
                    .parse()
                    .map_err(|e: std::num::ParseIntError| LspError::JsonRpcError {
                        code: -32700,
                        message: format!("Invalid Content-Length: {e}"),
                    })?;

            // 上限防护：超限报错而非按声明长度分配内存
            if content_length > MAX_MESSAGE_BYTES {
                return Err(LspError::JsonRpcError {
                    code: -32700,
                    message: format!(
                        "Content-Length {content_length} exceeds limit {MAX_MESSAGE_BYTES}"
                    ),
                });
            }

            // 读取剩余的头部行直到空行
            loop {
                header_line.clear();
                reader.read_line(&mut header_line).await?;
                if header_line.trim().is_empty() {
                    break;
                }
            }

            // 读取 body
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).await?;
            let body_str = String::from_utf8(body).map_err(|_| LspError::JsonRpcError {
                code: -32700,
                message: "Invalid UTF-8 in message body".to_string(),
            })?;
            return Ok(Some(body_str));
        }
        // 忽略其他头部行（如 Content-Type）
    }
}

#[cfg(test)]
#[path = "codec_test.rs"]
mod tests;
