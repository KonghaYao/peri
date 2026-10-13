//! 冻结 JSON 字节到 OTLP `stringValue` 的流式转义（契约 §3.4）。
//!
//! 全 crate 只有这一份转义实现：inline/file 来源与 none/gzip 编码共用它，因此分块读入不会
//! 切断字符——除了 `"`、`\` 与 `< 0x20` 控制字符，其余字节（含全部 UTF-8 多字节序列与续
//! 字节）一律原样批量透传，多字节字符跨块边界也只是两次透传。
//!
//! 转义口径与 serde_json 的字符串序列化逐字节一致（`none` 必须逐字节等同旧格式）：`"` 与
//! `\` 用短转义，`\b \f \n \r \t` 用短转义，其余控制字符用 `\u00xx`（小写十六进制）。
//!
//! 读侧错误按暂存端口分类上报；写侧错误保留 io 事实，由调用方按 sink 自身的失败状态
//! （限额/暂存）重新分类——这里只给不外泄正文的兜底描述。

use std::io::{Read, Write};

use crate::error::LangfuseError;
use crate::spool::SpoolError;

/// `\u00xx` 用的小写十六进制表。
const HEX: &[u8; 16] = b"0123456789abcdef";

/// 把 `reader` 中的冻结 JSON 字节写成**带引号**的 JSON 字符串字面量。
///
/// `chunk` 由调用方持有并可复用，允许任意大小（含 1 字节）；跨块结果与整块读入逐字节相同。
/// 空 chunk 是配置错误而不是静默的单字节循环。
pub(crate) fn write_json_string(
    reader: &mut dyn Read,
    out: &mut dyn Write,
    chunk: &mut [u8],
) -> Result<(), LangfuseError> {
    if chunk.is_empty() {
        return Err(LangfuseError::Config(
            "frozen JSON chunk buffer must be nonzero".into(),
        ));
    }
    out.write_all(b"\"").map_err(write_failure)?;
    loop {
        match reader.read(chunk) {
            Ok(0) => break,
            Ok(read) => escape_into(&chunk[..read], out).map_err(write_failure)?,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(LangfuseError::Spool(SpoolError::Io {
                    operation: "read frozen snapshot",
                    source: error,
                }))
            }
        }
    }
    out.write_all(b"\"").map_err(write_failure)
}

/// 写出带引号的小字符串（key/name/ID）；与 [`write_json_string`] 共用同一转义规则。
pub(crate) fn write_quoted(text: &str, out: &mut dyn Write) -> Result<(), LangfuseError> {
    out.write_all(b"\"").map_err(write_failure)?;
    escape_into(text.as_bytes(), out).map_err(write_failure)?;
    out.write_all(b"\"").map_err(write_failure)
}

/// 按 serde_json 口径转义一段字节（不含引号）；连续的非转义字节合并成一次写出。
pub(crate) fn escape_into(bytes: &[u8], out: &mut dyn Write) -> std::io::Result<()> {
    let mut plain = 0usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let mut unicode = [b'\\', b'u', b'0', b'0', 0u8, 0u8];
        let escape: &[u8] = match byte {
            b'"' => &b"\\\""[..],
            b'\\' => &b"\\\\"[..],
            0x08 => &b"\\b"[..],
            0x09 => &b"\\t"[..],
            0x0a => &b"\\n"[..],
            0x0c => &b"\\f"[..],
            0x0d => &b"\\r"[..],
            control if control < 0x20 => {
                unicode[4] = HEX[(control >> 4) as usize];
                unicode[5] = HEX[(control & 0x0f) as usize];
                &unicode[..]
            }
            _ => continue,
        };
        if plain < index {
            out.write_all(&bytes[plain..index])?;
        }
        out.write_all(escape)?;
        plain = index + 1;
    }
    if plain < bytes.len() {
        out.write_all(&bytes[plain..])?;
    }
    Ok(())
}

/// 边序列化边转义的适配器：serde_json 的紧凑输出直接进 sink，不为大对象物化整串。
pub(crate) struct EscapeWriter<'a> {
    out: &'a mut dyn Write,
}

impl<'a> EscapeWriter<'a> {
    pub(crate) fn new(out: &'a mut dyn Write) -> Self {
        Self { out }
    }
}

impl Write for EscapeWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        escape_into(bytes, self.out)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// sink 写入失败的兜底分类；不回显正文，真实分类由调用方按 sink 状态给出。
pub(crate) fn write_failure(error: std::io::Error) -> LangfuseError {
    LangfuseError::IngestionApi(format!("OTLP payload write failed: {error}"))
}
