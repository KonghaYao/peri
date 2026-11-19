//! 流图语句扫描、边标记与节点校验。

use super::StatementScan;

pub(super) fn scan_flow_statement(source: &str, start: usize, terminal: bool) -> StatementScan {
    let mut index = start;
    let mut quote = false;
    let mut escaped = false;
    let mut pipe = false;
    let mut comment = false;
    let mut depth: Vec<char> = Vec::new();

    while index < source.len() {
        let character = source[index..]
            .chars()
            .next()
            .expect("index stays on a UTF-8 boundary");
        let width = character.len_utf8();

        if comment {
            if character == '\n' {
                return StatementScan::Statement {
                    start,
                    text_end: index,
                    consumed_end: index + 1,
                };
            }
            index += width;
            continue;
        }
        if quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quote = false;
            }
            index += width;
            continue;
        }
        if pipe {
            if character == '|' {
                pipe = false;
            } else if character == '\n' {
                return StatementScan::Incomplete("unterminated edge label".to_string());
            }
            index += width;
            continue;
        }
        if let Some(expected) = depth.last().copied() {
            match character {
                '"' => quote = true,
                '[' => depth.push(']'),
                '(' => depth.push(')'),
                '{' => depth.push('}'),
                '>' => depth.push(']'),
                ']' | ')' | '}' if character == expected => {
                    depth.pop();
                }
                _ => {}
            }
            index += width;
            continue;
        }

        match character {
            '%' if source.as_bytes().get(index + 1) == Some(&b'%') => {
                comment = true;
                index += 2;
            }
            '"' => {
                quote = true;
                index += 1;
            }
            '[' => {
                depth.push(']');
                index += 1;
            }
            '(' => {
                depth.push(')');
                index += 1;
            }
            '{' => {
                depth.push('}');
                index += 1;
            }
            '>' => {
                depth.push(']');
                index += 1;
            }
            ']' | ')' | '}' => {
                index += 1;
            }
            '|' => {
                pipe = true;
                index += 1;
            }
            ';' => {
                return StatementScan::Statement {
                    start,
                    text_end: index,
                    consumed_end: index + 1,
                };
            }
            '\r' if source.as_bytes().get(index + 1) == Some(&b'\n') => {
                return StatementScan::Statement {
                    start,
                    text_end: index,
                    consumed_end: index + 2,
                };
            }
            '\n' => {
                return StatementScan::Statement {
                    start,
                    text_end: index,
                    consumed_end: index + 1,
                };
            }
            _ => {
                if let Some(edge_len) = match_edge_at(source, index) {
                    index += edge_len;
                } else {
                    index += width;
                }
            }
        }
    }

    if quote || pipe || !depth.is_empty() {
        return StatementScan::Incomplete("unterminated label or quote".to_string());
    }
    if terminal {
        return StatementScan::Statement {
            start,
            text_end: source.len(),
            consumed_end: source.len(),
        };
    }
    StatementScan::None
}

// ── 流图语句识别 ─────────────────────────────────────────────────────────

pub(super) enum FlowClass {
    Ignorable,
    Unknown,
    Header,
    Subgraph(String),
    End,
    Directive,
    Drawable,
}

pub(super) fn classify_flow(statement: &str) -> FlowClass {
    let trimmed = statement.trim();
    if trimmed.is_empty() || trimmed.starts_with("%%") {
        return FlowClass::Ignorable;
    }
    let cleaned = strip_inline_comment(trimmed).trim();
    if cleaned.is_empty() {
        return FlowClass::Ignorable;
    }
    let first = cleaned.split_whitespace().next().unwrap_or_default();
    let lower = first.to_ascii_lowercase();
    match lower.as_str() {
        "graph" | "flowchart" => {
            if valid_flow_header(cleaned) {
                FlowClass::Header
            } else {
                FlowClass::Unknown
            }
        }
        "subgraph" => FlowClass::Subgraph(subgraph_label(cleaned)),
        "end" => {
            if cleaned[first.len()..].trim().is_empty() {
                FlowClass::End
            } else {
                FlowClass::Unknown
            }
        }
        "direction" => {
            let direction = cleaned.split_whitespace().nth(1).unwrap_or_default();
            if valid_direction(direction) {
                FlowClass::Directive
            } else {
                FlowClass::Unknown
            }
        }
        "style" | "linkstyle" | "classdef" | "class" | "click" | "acctitle" | "accdescr" => {
            FlowClass::Directive
        }
        _ => {
            if validate_flow_chain(cleaned) {
                FlowClass::Drawable
            } else {
                FlowClass::Unknown
            }
        }
    }
}

fn valid_flow_header(statement: &str) -> bool {
    let mut parts = statement.split_whitespace();
    let Some(keyword) = parts.next() else {
        return false;
    };
    if !keyword.eq_ignore_ascii_case("graph") && !keyword.eq_ignore_ascii_case("flowchart") {
        return false;
    }
    match parts.next() {
        None => true,
        Some(direction) => valid_direction(direction),
    }
}

fn valid_direction(direction: &str) -> bool {
    matches!(
        direction.to_ascii_uppercase().as_str(),
        "LR" | "RL" | "TD" | "TB" | "BT"
    )
}

fn subgraph_label(statement: &str) -> String {
    let rest = statement
        .split_once(char::is_whitespace)
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();
    if let Some(open) = rest.find('[')
        && let Some(close) = rest[open + 1..].find(']')
    {
        return rest[open + 1..open + 1 + close]
            .trim()
            .trim_matches('"')
            .to_string();
    }
    rest.trim_matches('"').to_string()
}

pub(super) fn strip_inline_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote = false;
    let mut escaped = false;
    let mut index = 0;
    while index + 1 < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if quote && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quote = !quote;
        } else if !quote && byte == b'%' && bytes[index + 1] == b'%' {
            return &line[..index];
        }
        index += 1;
    }
    line
}

fn validate_flow_chain(statement: &str) -> bool {
    let mut tokens = FlowTokens {
        text: statement,
        pos: 0,
    };
    if !tokens.next_node_group() {
        return false;
    }
    loop {
        if !tokens.next_edge() {
            return tokens.at_end();
        }
        if !tokens.next_node_group() {
            return false;
        }
    }
}

struct FlowTokens<'a> {
    text: &'a str,
    pos: usize,
}

impl FlowTokens<'_> {
    fn skip_whitespace(&mut self) {
        while let Some(character) = self.text[self.pos..].chars().next() {
            if !character.is_whitespace() {
                break;
            }
            self.pos += character.len_utf8();
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_whitespace();
        self.pos >= self.text.len()
    }

    /// 节点组：`A & B` 扇出在同一侧共享后续边，与上游 0.58.0 的语法一致。
    fn next_node_group(&mut self) -> bool {
        if !self.next_node() {
            return false;
        }
        loop {
            self.skip_whitespace();
            if !self.text[self.pos..].starts_with('&') {
                return true;
            }
            self.pos += 1;
            if !self.next_node() {
                return false;
            }
        }
    }

    fn next_node(&mut self) -> bool {
        self.skip_whitespace();
        if self.pos >= self.text.len() {
            return false;
        }
        let start = self.pos;
        let mut quote = false;
        let mut escaped = false;
        let mut depth: Vec<char> = Vec::new();
        while self.pos < self.text.len() {
            let character = self.text[self.pos..]
                .chars()
                .next()
                .expect("pos stays on a UTF-8 boundary");
            let width = character.len_utf8();
            if quote {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quote = false;
                }
                self.pos += width;
                continue;
            }
            if let Some(expected) = depth.last().copied() {
                match character {
                    '"' => quote = true,
                    '[' => depth.push(']'),
                    '(' => depth.push(')'),
                    '{' => depth.push('}'),
                    '>' => depth.push(']'),
                    ']' | ')' | '}' if character == expected => {
                        depth.pop();
                    }
                    _ => {}
                }
                self.pos += width;
                continue;
            }
            if character.is_whitespace() {
                break;
            }
            if match_edge_at(self.text, self.pos).is_some() {
                break;
            }
            match character {
                '"' => quote = true,
                '[' => depth.push(']'),
                '(' => depth.push(')'),
                '{' => depth.push('}'),
                '>' => depth.push(']'),
                _ => {}
            }
            self.pos += width;
        }
        if quote || !depth.is_empty() {
            return false;
        }
        let token = self.text[start..self.pos].trim();
        !token.is_empty() && valid_node_token(token)
    }

    fn next_edge(&mut self) -> bool {
        self.skip_whitespace();
        let Some(length) = match_edge_at(self.text, self.pos) else {
            return false;
        };
        self.pos += length;
        true
    }
}

fn valid_node_token(token: &str) -> bool {
    let mut base = token;
    if let Some(index) = find_class_modifier(token) {
        base = &token[..index];
    }
    let base = base.trim();
    if base.is_empty() {
        return false;
    }
    let Some(open) = base.find(['[', '(', '{', '>']) else {
        return valid_node_id(base);
    };
    let id = base[..open].trim();
    if !valid_node_id(id) {
        return false;
    }
    balanced_shape(&base[open..])
}

fn valid_node_id(id: &str) -> bool {
    !id.is_empty()
        && !id.chars().any(|character| {
            character.is_whitespace()
                || matches!(
                    character,
                    ';' | '"' | '|' | '&' | '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>'
                )
        })
}

fn find_class_modifier(token: &str) -> Option<usize> {
    let mut quote = false;
    let mut escaped = false;
    let mut depth: Vec<char> = Vec::new();
    let bytes = token.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        let character = token[index..]
            .chars()
            .next()
            .expect("index stays on a UTF-8 boundary");
        if quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quote = false;
            }
            index += character.len_utf8();
            continue;
        }
        if let Some(expected) = depth.last().copied() {
            match character {
                '"' => quote = true,
                '[' => depth.push(']'),
                '(' => depth.push(')'),
                '{' => depth.push('}'),
                ']' | ')' | '}' if character == expected => {
                    depth.pop();
                }
                _ => {}
            }
            index += character.len_utf8();
            continue;
        }
        match character {
            '"' => quote = true,
            '[' => depth.push(']'),
            '(' => depth.push(')'),
            '{' => depth.push('}'),
            ':' if bytes.get(index + 1) == Some(&b':') && bytes.get(index + 2) == Some(&b':') => {
                return Some(index);
            }
            _ => {}
        }
        index += character.len_utf8();
    }
    None
}

fn balanced_shape(shape: &str) -> bool {
    let mut quote = false;
    let mut escaped = false;
    let mut depth: Vec<char> = Vec::new();
    let mut index = 0;
    while index < shape.len() {
        let character = shape[index..]
            .chars()
            .next()
            .expect("index stays on a UTF-8 boundary");
        let width = character.len_utf8();
        if quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quote = false;
            }
            index += width;
            continue;
        }
        match character {
            '"' => quote = true,
            '[' => depth.push(']'),
            '(' => depth.push(')'),
            '{' => depth.push('}'),
            '>' => depth.push(']'),
            ']' | ')' | '}' => {
                if depth.last().copied() != Some(character) {
                    return false;
                }
                depth.pop();
            }
            _ => {}
        }
        index += width;
    }
    !quote && depth.is_empty()
}

/// 尝试在 `index` 处匹配一条流图边标记，返回其字节长度。
///
/// 带标签的边整段消费，标签里的 `;` 不再被当成语句分隔符。
fn match_edge_at(text: &str, index: usize) -> Option<usize> {
    if index >= text.len() {
        return None;
    }
    if let Some(length) = match_labeled_edge(text, index) {
        return Some(length);
    }
    let rest = &text[index..];
    let first = rest.chars().next()?;

    if first == '<' {
        let mut cursor = index + 1;
        let mut dashes = 0;
        while let Some(character) = text[cursor..].chars().next() {
            match character {
                '-' => {
                    dashes += 1;
                    cursor += 1;
                }
                '.' => cursor += 1,
                _ => break,
            }
        }
        if dashes < 2 {
            return None;
        }
        if let Some(character) = text[cursor..].chars().next()
            && matches!(character, '>' | 'o' | 'x')
        {
            cursor += character.len_utf8();
        }
        return extend_pipe_label(text, index, cursor);
    }
    if first == 'o' || first == 'x' {
        let mut cursor = index + first.len_utf8();
        let mut dashes = 0;
        while text[cursor..].starts_with('-') {
            dashes += 1;
            cursor += 1;
        }
        if dashes < 2 {
            return None;
        }
        if let Some(character) = text[cursor..].chars().next()
            && matches!(character, 'o' | 'x' | '>')
        {
            cursor += character.len_utf8();
        }
        return extend_pipe_label(text, index, cursor);
    }
    if first == '-' || first == '=' {
        let marker = first;
        let mut cursor = index;
        let mut marker_count = 0;
        let mut dotted = false;
        while text[cursor..].starts_with(marker) {
            marker_count += 1;
            cursor += 1;
        }
        if marker == '-' && text[cursor..].starts_with('.') {
            dotted = true;
            cursor += 1;
            while text[cursor..].starts_with('-') {
                marker_count += 1;
                cursor += 1;
            }
        }
        if marker == '-' && marker_count < 2 {
            return None;
        }
        if marker == '=' && marker_count < 2 {
            return None;
        }
        if marker == '-' && !dotted && marker_count < 2 {
            // 单个 `-` 属于节点 ID 或半条箭头，不是完整边。
            return None;
        }
        if marker == '-' && !dotted && marker_count == 2 {
            // `--` 是带标签边的开头；没有闭合箭头时不当作完整边。
            let next = text[cursor..].chars().next();
            if !matches!(next, Some('>' | 'o' | 'x' | '-')) {
                return None;
            }
        }
        if let Some(character) = text[cursor..].chars().next()
            && matches!(character, '>' | 'o' | 'x')
        {
            cursor += character.len_utf8();
        }
        return extend_pipe_label(text, index, cursor);
    }
    if first == '~' {
        let mut cursor = index;
        let mut count = 0;
        while text[cursor..].starts_with('~') {
            count += 1;
            cursor += 1;
        }
        if count >= 3 {
            return extend_pipe_label(text, index, cursor);
        }
    }
    None
}

/// 紧跟在箭头后的 `|label|` 属于边；未闭合时保持语句不完整。
fn extend_pipe_label(text: &str, start: usize, end: usize) -> Option<usize> {
    if !text[end..].starts_with('|') {
        return Some(end - start);
    }
    let rest = &text[end + 1..];
    let close = rest.find('|')?;
    if rest[..close].contains('\n') {
        return None;
    }
    Some(end + 1 + close + 1 - start)
}

fn match_labeled_edge(text: &str, index: usize) -> Option<usize> {
    let rest = &text[index..];
    let (open_len, closers): (usize, &[&str]) = if rest.starts_with("--") {
        (2, &["-->", "---", "--o", "--x"])
    } else if rest.starts_with("-.") {
        (2, &[".->", ".-"])
    } else if rest.starts_with("==") {
        (2, &["==>", "===", "==o", "==x"])
    } else {
        return None;
    };
    let after = rest[open_len..].chars().next()?;
    if !after.is_whitespace() {
        return None;
    }
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let label = &rest[open_len..line_end];
    if label.trim().is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    for closer in closers {
        if let Some(offset) = label.find(closer) {
            if offset == 0 {
                continue;
            }
            let end = open_len + offset + closer.len();
            if best.is_none_or(|(best_end, _)| end < best_end) {
                best = Some((end, label[..offset].trim().len()));
            }
        }
    }
    let Some((end, label_len)) = best else {
        // 标签可能还在下一个 token 里：先吞到行尾，让语句保持完整；
        // 换行后的分类会把它判为未知语句，EOF 时仍返回不完整。
        return Some(line_end);
    };
    if label_len == 0 {
        return None;
    }
    Some(end)
}
