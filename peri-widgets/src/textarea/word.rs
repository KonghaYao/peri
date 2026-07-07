/// 字符分类：用于词边界导航（借鉴 tui-textarea 的 Space/Punct/Other 三分法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharCategory {
    /// 所有 Unicode 空白字符
    Space,
    /// ASCII 标点 + CJK 全角标点
    Punct,
    /// 字母、数字、CJK 汉字及其他
    Other,
}

/// 将字符分类为 Space/Punct/Other。
pub fn classify_char(ch: char) -> CharCategory {
    if ch.is_whitespace() {
        return CharCategory::Space;
    }
    if ch.is_ascii_punctuation() {
        return CharCategory::Punct;
    }
    // CJK 全角标点范围
    if matches!(ch,
        '\u{3000}'..='\u{303F}' |  // CJK 符号标点（含、。「」等）
        '\u{FF01}'..='\u{FF0F}' |  // 全角感叹号~全角除号（含，）
        '\u{FF1A}'..='\u{FF20}' |  // 全角冒号~全角@
        '\u{FF3B}'..='\u{FF40}' |  // 全角左方括号~全角反引号
        '\u{FF5B}'..='\u{FF65}' |  // 全角左花括号~半角片假名中点
        '\u{FE50}'..='\u{FE6F}'    // 小型变体形式
    ) {
        return CharCategory::Punct;
    }
    CharCategory::Other
}

/// 向前（向文本首部）查找词边界。
/// 从 cursor 位置开始，跳过同类别字符，再跳过前一类字符，停在类别改变的边界。
pub fn prev_word_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let cur = cursor.min(total);
    if cur == 0 {
        return 0;
    }

    let mut pos = cur;
    // 跳过当前字符的同类
    let cur_cat = classify_char(chars[pos.saturating_sub(1)]);
    while pos > 0 && classify_char(chars[pos - 1]) == cur_cat {
        pos -= 1;
    }
    // 跳过中间的空格
    while pos > 0 && classify_char(chars[pos - 1]) == CharCategory::Space {
        pos -= 1;
    }
    // 跳过词本身的字符
    if pos > 0 {
        let word_cat = classify_char(chars[pos - 1]);
        while pos > 0 && classify_char(chars[pos - 1]) == word_cat {
            pos -= 1;
        }
    }
    pos
}

/// 向后（向文本尾部）查找词边界。
pub fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    if cursor >= total {
        return total;
    }

    let mut pos = cursor;
    // 跳过当前字符的同类
    let cur_cat = classify_char(chars[pos]);
    while pos < total && classify_char(chars[pos]) == cur_cat {
        pos += 1;
    }
    // 跳过空格
    while pos < total && classify_char(chars[pos]) == CharCategory::Space {
        pos += 1;
    }
    pos
}
