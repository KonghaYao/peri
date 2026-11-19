//! Mermaid 围栏元数据：从原始 Markdown 提取 CommonMark fenced code block
//! 的种类、长度、原文范围、真实闭合状态与可直接交给 Mermaid 的正文。
//!
//! [Why 独立扫描] `ParsedBlock::CodeBlock` 不携带原文位置，展示管线还会在
//! `ensure_closed_code_fences` 里临时补结束符——无法区分「真实闭合」与「补齐」。
//! 这里复用与 `ratatui-kit-markdown` 相同的 pulldown-cmark 选项与事件范围，
//! 在预处理之前扫描**原始输入**，让范围与身份始终使用原文坐标。

use pulldown_cmark_012::CodeBlockKind;
use pulldown_cmark_012::Event;
use pulldown_cmark_012::Parser;
use pulldown_cmark_012::Tag;
use pulldown_cmark_012::TagEnd;

pub use crate::mermaid::fence::{
    FenceClose, FenceKind, MermaidFence, MermaidFenceKey, MermaidMessageIdentity,
    MermaidMessageRef, is_mermaid_info,
};

fn first_info_token(info: &str) -> String {
    info.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

struct PendingFence {
    start: usize,
    end: usize,
    info: String,
    source: String,
    last_text_end: Option<usize>,
}

impl PendingFence {
    fn finish(self, input: &str, terminal: bool) -> Option<MermaidFence> {
        if !is_mermaid_info(&self.info) {
            return None;
        }
        let bytes = input.as_bytes();
        let marker = *bytes.get(self.start)?;
        let kind = match marker {
            b'`' => FenceKind::Backtick,
            b'~' => FenceKind::Tilde,
            _ => return None,
        };
        let marker_len = bytes[self.start..self.end]
            .iter()
            .take_while(|byte| **byte == marker)
            .count();
        if marker_len < 3 {
            return None;
        }

        let opener_line_end = input[self.start..self.end]
            .find('\n')
            .map(|offset| self.start + offset);
        let opener_confirmed = opener_line_end.is_some() || terminal;
        let line_end = opener_line_end.unwrap_or(self.end);
        let line_start = input[..self.start]
            .rfind('\n')
            .map_or(0, |offset| offset + 1);
        let opening_line = input[line_start..line_end]
            .strip_suffix('\r')
            .unwrap_or(&input[line_start..line_end])
            .to_string();
        let body_start = if line_end < self.end {
            line_end + 1
        } else {
            self.end
        };
        let mut body_end = self.last_text_end.unwrap_or(body_start);
        let mut end = self.end;
        let mut close = FenceClose::Open;
        if body_end < self.end {
            let trailing = &input[body_end..self.end];
            if is_closing_line(trailing, marker, marker_len) {
                // pulldown 的块范围可能不含关闭行末尾换行；先把它补回原文范围，
                // 再以「关闭行是否真正换行结束」区分已确认与暂态。
                let mut line_terminated = input[..end].ends_with('\n');
                if !line_terminated && let Some(rest) = input.get(end..) {
                    let (terminator, advance) = if rest.starts_with("\r\n") {
                        (true, 2)
                    } else if rest.starts_with('\n') || rest.starts_with('\r') {
                        (true, 1)
                    } else {
                        (false, 0)
                    };
                    line_terminated = terminator;
                    end = end.saturating_add(advance);
                }
                close = if terminal || line_terminated {
                    FenceClose::Closed
                } else {
                    FenceClose::Provisional
                };
            } else {
                body_end = self.end;
            }
        }

        Some(MermaidFence {
            kind,
            marker_len,
            language: first_info_token(&self.info),
            info: self.info,
            open_start: self.start,
            body_start,
            body_end,
            end,
            close,
            opener_confirmed,
            opening_line,
            raw: input[body_start..body_end].to_string(),
            source: self.source,
        })
    }
}

/// 关闭行判定：去掉可选的换行后，最后一个非空格段必须是不少于开围栏长度的
/// 同类标记，且其后只有空格。容器前缀留在标记之前，不影响该判定。
fn is_closing_line(trailing: &str, marker: u8, marker_len: usize) -> bool {
    let line = trailing.strip_suffix('\n').unwrap_or(trailing);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let line = line.trim_end_matches(' ');
    line.as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == marker)
        .count()
        >= marker_len
}

/// 扫描原文中的所有 Mermaid fenced code block。
///
/// `terminal` 为 true 表示输入已经完整结束：末尾没有换行的关闭行可以确认。
/// 流式渲染传 false，让尚可被下一个 token 延长的关闭行保持暂态。
pub(super) fn scan_mermaid_fences(input: &str, terminal: bool) -> Vec<MermaidFence> {
    // 绝大多数消息没有 mermaid；避免为每条消息再跑一遍完整 pulldown 解析。
    if !input
        .as_bytes()
        .windows(b"mermaid".len())
        .any(|window| window.eq_ignore_ascii_case(b"mermaid"))
    {
        return Vec::new();
    }
    let options = super::scan::md_options();
    let mut fences = Vec::new();
    let mut pending: Option<PendingFence> = None;
    for (event, range) in Parser::new_ext(input, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                pending = Some(PendingFence {
                    start: range.start,
                    end: range.end,
                    info: info.into_string(),
                    source: String::new(),
                    last_text_end: None,
                });
            }
            Event::Text(text) => {
                if let Some(pending) = pending.as_mut() {
                    pending.source.push_str(text.as_ref());
                    pending.last_text_end = Some(range.end);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(pending) = pending.take()
                    && let Some(fence) = pending.finish(input, terminal)
                {
                    fences.push(fence);
                }
            }
            _ => {}
        }
    }
    fences
}

/// 把 Mermaid 围栏元数据按 fenced code block 出现顺序绑定到 `ParsedBlock`。
///
/// 返回值与 blocks 等长；只有信息串首个标记为 `mermaid` 的块带元数据。
/// 缩进代码块或普通代码块不会消费围栏列表。
pub(super) fn bind_fences(
    blocks: &[ratatui_kit_markdown::ParsedBlock],
    fences: &[MermaidFence],
) -> Vec<Option<MermaidFence>> {
    let mut by_block = vec![None; blocks.len()];
    let mut fences = fences.iter();
    for (index, block) in blocks.iter().enumerate() {
        let ratatui_kit_markdown::ParsedBlock::CodeBlock(language, _) = block else {
            continue;
        };
        if !is_mermaid_info(language) {
            continue;
        }
        let Some(fence) = fences.next() else {
            break;
        };
        debug_assert_eq!(fence.language.to_ascii_lowercase(), "mermaid");
        by_block[index] = Some(fence.clone());
    }
    by_block
}
