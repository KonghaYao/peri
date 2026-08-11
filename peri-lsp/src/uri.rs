//! `file://` URI 与文件系统路径的互转。
//!
//! LSP 协议要求 `rootUri` / `textDocument.uri` 是完整 URI 而非裸路径。
//! 相对路径直接拼接 `file://` 会得到 `file://rel`，在 RFC 3986 下 `rel`
//! 被当作 authority，空格/非 ASCII 字符导致服务器端 parse 失败。本模块
//! 提供绝对化 + percent-encode 的完整转换。

use std::path::Path;

/// 将文件系统路径转换为 `file://` URI。
///
/// - 输入已是 `file://` 前缀时原样返回（幂等）；
/// - 相对路径基于当前工作目录绝对化（含 `..` 归一化）；
/// - 空格、非 ASCII、`#`、`?`、`%` 等字符按 RFC 3986 percent-encode，
///   保留 `/` 分隔符与 `-._~` unreserved 字符。
///
/// 路径语义按 Unix 约定（`/` 分隔）；Windows drive letter 不在处理范围内。
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.starts_with("file://") {
        return s.into_owned();
    }

    // 相对路径基于当前工作目录绝对化；失败（如无 cwd）时保留原样
    let abs = match std::path::absolute(path) {
        Ok(abs) => abs.to_string_lossy().into_owned(),
        Err(_) => s.into_owned(),
    };

    format!("file://{}", percent_encode(&abs))
}

/// 将 `file://` URI 转换回文件系统路径：去除前缀 + percent-decode。
///
/// 输入无 `file://` 前缀时原样返回。非法的 `%` 序列（不是 `%XX` 十六进制）
/// 原样保留，不产生错误。
pub fn uri_to_path(uri: &str) -> String {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    percent_decode(rest)
}

/// RFC 3986 percent-encode：仅保留 unreserved 字符与 `/` 分隔符。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// percent-decode；非 `%XX` 的 `%` 序列原样保留。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "uri_test.rs"]
mod tests;
