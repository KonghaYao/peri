use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenUiBlock {
    pub source: String,
    pub raw: String,
    /// UTF-8 字节区间，包含 fence 行及其换行。
    pub raw_range: Range<usize>,
    /// UTF-8 字节区间，不包含关闭 fence；开头同行代码也包含在内。
    pub source_range: Range<usize>,
    pub closed: bool,
    pub opener_code: Option<String>,
}

struct Fence {
    marker: char,
    length: usize,
    tag: String,
    code_start: Option<usize>,
}
struct Open {
    start: usize,
    source_start: usize,
    fence: Fence,
    quote: Option<char>,
}

pub fn scan_blocks(input: &str) -> Vec<GenUiBlock> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    let mut open: Option<Open> = None;
    for line in input.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        if let Some(current) = &mut open {
            let closing = current.quote.is_none() && closes(text, &current.fence);
            if closing {
                if current.fence.tag.eq_ignore_ascii_case("gen-ui") {
                    blocks.push(block(input, current, offset, offset + line.len(), true));
                }
                open = None;
            } else if current.fence.tag.eq_ignore_ascii_case("gen-ui") {
                scan_quote(line, &mut current.quote);
            }
        } else if let Some(fence) = opener(text) {
            let source_start = fence.code_start.map_or(offset + line.len(), |i| offset + i);
            let mut quote = None;
            if fence.tag.eq_ignore_ascii_case("gen-ui") && fence.code_start.is_some() {
                scan_quote(&input[source_start..offset + line.len()], &mut quote);
            }
            open = Some(Open {
                start: offset,
                source_start,
                fence,
                quote,
            });
        }
        offset += line.len();
    }
    if let Some(open) = open {
        if open.fence.tag.eq_ignore_ascii_case("gen-ui") {
            blocks.push(block(input, &open, input.len(), input.len(), false));
        }
    }
    blocks
}

fn block(input: &str, open: &Open, source_end: usize, raw_end: usize, closed: bool) -> GenUiBlock {
    let source_range = open.source_start..source_end;
    let raw_range = open.start..raw_end;
    let opener_code = open.fence.code_start.map(|_| {
        input[open.source_start..]
            .split('\n')
            .next()
            .unwrap_or("")
            .trim_end_matches('\r')
            .to_owned()
    });
    GenUiBlock {
        source: input[source_range.clone()].to_owned(),
        raw: input[raw_range.clone()].to_owned(),
        source_range,
        raw_range,
        closed,
        opener_code,
    }
}

fn opener(text: &str) -> Option<Fence> {
    let indent = text.len() - text.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let trimmed = &text[indent..];
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed.chars().take_while(|c| *c == marker).count();
    if length < 3 {
        return None;
    }
    let rest = trimmed[length..].trim_start();
    if rest.contains("```") {
        return None;
    }
    let tag_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let tag = rest[..tag_len].to_owned();
    let code = rest[tag_len..].trim();
    let code_start = (!code.is_empty()).then(|| text.trim_end().len() - code.len());
    Some(Fence {
        marker,
        length,
        tag,
        code_start,
    })
}

fn closes(text: &str, fence: &Fence) -> bool {
    let indent = text.len() - text.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let trimmed = &text[indent..];
    let length = trimmed.chars().take_while(|c| *c == fence.marker).count();
    length >= fence.length && trimmed[length..].trim().is_empty()
}

fn scan_quote(line: &str, quote: &mut Option<char>) {
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = *quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                *quote = None;
            }
        } else if matches!(c, '\'' | '"') {
            *quote = Some(c);
        } else if c == '#' || (c == '/' && chars.peek() == Some(&'/')) {
            break;
        }
    }
}
