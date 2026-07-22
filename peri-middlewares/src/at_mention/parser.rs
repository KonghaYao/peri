use serde::{Deserialize, Serialize};

/// @ 提及解析结果
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AtMention {
    pub path: String,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
}

/// 从文本中提取 @ 提及
///
/// 支持格式：
/// - `@path/to/file.rs` — 普通路径
/// - `@"path/with spaces/file.rs"` — 带引号路径
/// - `@file.rs#L10` — 单行
/// - `@file.rs#L10-20` — 行范围
///
/// 跳过：email 格式（user@example.com）、单字符提及（@a）
pub fn extract_at_mentions(text: &str) -> Vec<AtMention> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // 查找 @ 字符
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }

        // @ 前面不能是单词字符（排除 email）
        if i > 0 && is_word_char(bytes[i - 1]) {
            i += 1;
            continue;
        }

        let at_pos = i;
        i += 1; // 跳过 @

        // 解析路径
        let path;
        if i < len && bytes[i] == b'"' {
            // 带引号路径
            i += 1; // 跳过开头引号
            let start = i;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            path = text[start..i].to_string();
            if i < len {
                i += 1; // 跳过结尾引号
            }
        } else {
            // 普通路径：匹配到空白或文本结束
            let start = i;
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            path = text[start..i].to_string();
        }

        // 路径必须至少 2 个字符
        if path.len() < 2 {
            continue;
        }

        // 跳过 email 格式：路径中不含 / 且 @ 后面紧接着路径且后面有 . 和 TLD
        // 简单判断：路径不含 / 且后跟 . 和常见 TLD
        if !path.contains('/') && !path.contains('\\') {
            // 检查是否 email 模式：前面有字母，后面有 .xxx
            if at_pos > 0 && is_word_char(bytes[at_pos - 1]) {
                continue;
            }
        }

        // 解析行号 #L10 或 #L10-20
        let (path, line_start, line_end) = parse_line_suffix(&path);

        let mention = AtMention {
            path,
            line_start,
            line_end,
        };

        // 去重
        if seen.insert(mention.clone()) {
            results.push(mention);
        }
    }

    results
}

/// 从路径中解析 #L10 或 #L10-20 行号后缀
fn parse_line_suffix(path: &str) -> (String, Option<usize>, Option<usize>) {
    if let Some(hash_pos) = path.rfind('#') {
        let suffix = &path[hash_pos + 1..];
        let prefix = &path[..hash_pos];

        if let Some(rest) = suffix.strip_prefix('L') {
            // 单行 #L10 或范围 #L10-20
            if let Some(dash_pos) = rest.find('-') {
                if let (Ok(start), Ok(end)) = (
                    rest[..dash_pos].parse::<usize>(),
                    rest[dash_pos + 1..].parse::<usize>(),
                ) {
                    return (prefix.to_string(), Some(start), Some(end));
                }
            } else if let Ok(line) = rest.parse::<usize>() {
                return (prefix.to_string(), Some(line), None);
            }
        }
    }

    (path.to_string(), None, None)
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

#[cfg(test)]
#[path = "parser_test.rs"]
mod tests;
