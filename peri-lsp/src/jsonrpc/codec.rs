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

/// Content-Length 分帧解码
///
/// 读取 `"Content-Length: {N}\r\n\r\n"` 头部行，然后读取 N 字节 body。
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
        if let Some(content_length_str) = trimmed.strip_prefix("Content-Length:") {
            let content_length: usize =
                content_length_str
                    .trim()
                    .parse()
                    .map_err(|e: std::num::ParseIntError| LspError::JsonRpcError {
                        code: -32700,
                        message: format!("Invalid Content-Length: {e}"),
                    })?;

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
