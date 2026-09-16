//! 投影哨兵 V1 语法（`peri-acp-types/src/sentinel.rs` 的等价自持实现）。
//!
//! 源实现把「`... [{N} 字符已省略] ...`」这类投影占位行视为受保护文本：Write/Edit 提交前
//! 比较 pre/post 的哨兵计数，任何**新增**都拒绝提交（`transaction.rs:70`），防止模型把
//! 自己的截断标记写回文件。独立项目不得依赖 `peri-acp-types`，因此在此逐字复刻语法：
//!
//! - 整行必须形如 `... [` + 一个或多个 ASCII 数字 + ` 字符已省略] ...`；
//! - LF、CRLF 与输入结尾都终止一行，**裸 CR 不终止**；
//! - 计数按行的**规范正文**（去掉行尾 `\r`）分桶，数字串不解析为整数。
//!
//! 与源实现唯一的表示差异：用 `BTreeMap`（而非 `HashMap`）以获得确定性遍历顺序，判定结果
//! 不变（只关心「是否存在计数增加的桶」）。

use std::collections::BTreeMap;

/// 哨兵前缀（V1）。
const PREFIX_V1: &str = "... [";
/// 哨兵后缀（V1）。
const SUFFIX_V1: &str = " 字符已省略] ...";

/// 逐行统计 V1 哨兵出现次数，键为行的规范正文。
pub fn sentinel_counts_v1(text: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let bytes = text.as_bytes();
    let mut line_start = 0usize;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line_end = if index > line_start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        count_line(&text[line_start..line_end], &mut counts);
        line_start = index + 1;
    }

    if line_start < text.len() {
        count_line(&text[line_start..], &mut counts);
    }
    counts
}

/// `post` 是否比 `pre` 多出任何 V1 哨兵行。
pub fn introduces_sentinel_v1(pre: &str, post: &str) -> bool {
    grows(post, &sentinel_counts_v1(pre))
}

/// 字节形态（Write/Edit 的提交路径：非 UTF-8 行被跳过，与源实现一致）。
pub fn introduces_sentinel_bytes(pre: &[u8], post: &[u8]) -> bool {
    grows_bytes(post, &counts_bytes(pre))
}

fn grows(post: &str, pre_counts: &BTreeMap<String, usize>) -> bool {
    sentinel_counts_v1(post)
        .into_iter()
        .any(|(sentinel, count)| count > pre_counts.get(&sentinel).copied().unwrap_or(0))
}

fn grows_bytes(post: &[u8], pre_counts: &BTreeMap<String, usize>) -> bool {
    counts_bytes(post)
        .into_iter()
        .any(|(sentinel, count)| count > pre_counts.get(&sentinel).copied().unwrap_or(0))
}

fn counts_bytes(bytes: &[u8]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        count_line(line, &mut counts);
    }
    counts
}

fn count_line(line: &str, counts: &mut BTreeMap<String, usize>) {
    if !is_sentinel_v1(line) {
        return;
    }
    *counts.entry(line.to_string()).or_default() += 1;
}

fn is_sentinel_v1(line: &str) -> bool {
    let Some(rest) = line.strip_prefix(PREFIX_V1) else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(SUFFIX_V1) else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "... [42 字符已省略] ...";

    #[test]
    fn test_detects_new_sentinel_lines() {
        assert!(introduces_sentinel_v1("a\n", &format!("a\n{SENTINEL}\n")));
        assert!(!introduces_sentinel_v1(
            &format!("{SENTINEL}\n"),
            &format!("{SENTINEL}\n")
        ));
        assert!(introduces_sentinel_v1(
            &format!("{SENTINEL}\n"),
            &format!("{SENTINEL}\n{SENTINEL}\n")
        ));
    }

    #[test]
    fn test_counts_identical_lines_as_one_bucket() {
        let text = format!("{SENTINEL}\n{SENTINEL}\nother\n{SENTINEL}");
        let counts = sentinel_counts_v1(&text);
        assert_eq!(counts.get(SENTINEL), Some(&3));
        assert_eq!(counts.len(), 1);
    }

    #[test]
    fn test_requires_exact_lexeme() {
        for line in [
            ".. [1 字符已省略] ...",
            "... [1 字符已省略] ..",
            "... [] 字符已省略] ...",
            "... [1 字符已省略] ... tail",
            "  ... [1 字符已省略] ...",
            "... [1字数已省略] ...",
        ] {
            assert!(!is_sentinel_v1(line), "{line:?} 不应被识别为 V1 哨兵");
        }
        assert!(is_sentinel_v1("... [0 字符已省略] ..."));
        assert!(is_sentinel_v1("... [0001 字符已省略] ..."));
    }

    #[test]
    fn test_crlf_is_stripped_and_bare_cr_does_not_split() {
        assert!(introduces_sentinel_v1("a", &format!("a\r\n{SENTINEL}\r\n")));
        // 裸 CR 不终止行：整行不是哨兵
        assert!(!is_sentinel_v1(&format!("\r{SENTINEL}")));
    }

    #[test]
    fn test_byte_variant_skips_non_utf8_lines() {
        let mut post = vec![0xffu8, b'\n'];
        post.extend_from_slice(format!("{SENTINEL}\n").as_bytes());
        assert!(introduces_sentinel_bytes(b"x\n", &post));
        assert!(!introduces_sentinel_bytes(&post, &post));
    }
}
