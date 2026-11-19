//! Terminal-facing sanitization shared by Markdown image rendering.
//!
//! 展示字段进终端前剥离控制字符，并对 URL scheme 分类（远程图片不自动下载）。
//! 文件路径与图片字节校验属于应用层（`peri-tui`），不在此模块。

use std::borrow::Cow;

/// URL scheme 分类：`file://` 与无 scheme 的相对/绝对路径 → [`UrlKind::Local`]
/// （Local 仍需路径校验）；`http`/`https` → [`UrlKind::RemoteHttp`]；其余 scheme
/// （`javascript:`、`data:`、`ftp:` 等）→ [`UrlKind::Dangerous`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlKind {
    /// `file://` 与无 scheme 的相对/绝对路径（Local 仍需路径校验）。
    Local,
    /// `http` / `https`——默认不自动下载，只显示文本/链接。
    RemoteHttp,
    /// 其余 scheme（`javascript:`、`data:`、`ftp:` 等）——只显示文本/链接。
    Dangerous,
}

/// 展示字段进终端前过滤：剥离 C0（保留 `\n` `\t`）、ESC、C1、DEL；
/// 其余不可见/不可打印字符（零宽、bidi 控制、软连字符等）可见化为 `�`。
///
/// `\u{200c}`（ZWNJ）/`\u{200d}`（ZWJ）**原样保留**：它们是显示宽度 0 的合法
/// 组合符（emoji ZWJ 序列的组成部分），非注入向量；bidi 威胁由
/// `\u{202a}..=\u{202e}` 覆盖。
///
/// 返回 [`Cow`]：无控制字符时零拷贝借用。
pub fn sanitize_for_terminal(s: &str) -> Cow<'_, str> {
    // 快速路径：第一个需要处理的字符之前直接借用。
    let Some((first, _)) = s.char_indices().find(|(_, c)| needs_sanitization(*c)) else {
        return Cow::Borrowed(s);
    };
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..first]);
    for c in s[first..].chars() {
        match c {
            '\n' | '\t' => out.push(c),
            // C0（除 \n\t）、ESC、C1、DEL → 剥离（防 escape 序列注入终端）。
            '\u{0000}'..='\u{001f}' | '\u{007f}' | '\u{0080}'..='\u{009f}' => {}
            // 其余不可见/不可打印字符 → 可见化：软连字符、零宽系列（ZWSP/
            // LRM/RLM——ZWNJ/ZWJ 除外，见函数文档）、bidi 控制、LS/PS、
            // 通用格式字符、BOM。
            '\u{00ad}'
            | '\u{200b}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{2060}'..='\u{2064}'
            | '\u{feff}' => out.push('\u{fffd}'),
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// 该字符是否需要被 [`sanitize_for_terminal`] 处理（与过滤循环保持一致）。
#[must_use]
fn needs_sanitization(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        '\u{0000}'..='\u{001f}' | '\u{007f}' | '\u{0080}'..='\u{009f}' => true,
        '\u{00ad}'
        | '\u{200b}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{2060}'..='\u{2064}'
        | '\u{feff}' => true,
        _ => false,
    }
}

/// URL scheme 分类：`file://` 与无 scheme 的相对/绝对路径 → [`UrlKind::Local`]
/// （Local 仍需路径校验）；`http`/`https` → [`UrlKind::RemoteHttp`]；其余 scheme
/// （`javascript:`、`data:`、`ftp:` 等）→ [`UrlKind::Dangerous`]。
///
/// Windows 盘符（`C:\foo` / `C:/foo`）是本地路径，不按 scheme 处理。
pub fn classify_url(url: &str) -> UrlKind {
    // RFC 3986 scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"
    let colon = url.find(':').filter(|&colon| {
        let mut chars = url[..colon].chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    let Some(colon) = colon else {
        // 无 scheme：相对/绝对路径。
        return UrlKind::Local;
    };
    let scheme = url[..colon].to_ascii_lowercase();
    // 单字母 + 路径分隔符 → Windows 盘符（本地路径，非 scheme）。
    if scheme.len() == 1 && url[colon + 1..].starts_with(['\\', '/']) {
        return UrlKind::Local;
    }
    match scheme.as_str() {
        "file" => UrlKind::Local,
        "http" | "https" => UrlKind::RemoteHttp,
        _ => UrlKind::Dangerous,
    }
}

#[cfg(test)]
mod tests {
    use super::{UrlKind, classify_url, sanitize_for_terminal};

    #[test]
    fn strips_escape_sequences_and_keeps_layout_whitespace() {
        assert_eq!(sanitize_for_terminal("a\u{1b}[31mb"), "a[31mb");
        assert_eq!(sanitize_for_terminal("a\n\tb"), "a\n\tb");
    }

    #[test]
    fn classifies_url_schemes() {
        assert_eq!(classify_url("./a.png"), UrlKind::Local);
        assert_eq!(classify_url("C:/a.png"), UrlKind::Local);
        assert_eq!(
            classify_url("https://example.com/a.png"),
            UrlKind::RemoteHttp
        );
        assert_eq!(classify_url("javascript:alert(1)"), UrlKind::Dangerous);
    }
}
