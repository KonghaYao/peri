use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_kit_markdown::{MarkdownTheme, ParsedBlock};

use super::code_block::code_block_lines;
use super::heading::heading_line;
use super::list::list_item_line;
use super::span_style::apply_span_styles;
use super::table::compute_table_col_widths;
use super::types::{MarkdownSegment, TableData};

// ── 块级转换 ────────────────────────────────────────────────────────

/// convert 过程中的累积状态，可序列化为缓存以便续跑。
///
/// [Why] 流式 markdown 渲染期间，每个 token 触发整段 text 的 convert。
/// 实际上前面已闭合的 blocks（paragraph / list item / code block / table）
/// 内容完全不变——只需处理新增 block。
///
/// [续跑契约] 调用方必须保证：传入的 `state.processed_block_count` 对应到
/// 当前 `blocks` 前 N 个，且这 N 个 block 的内容与上次缓存时一致。
/// 这通过 `text.starts_with(cache.stable_text)` + `cache.stable_text` 以
/// 换行符结尾（保证最后一个 block 已闭合）来保证。
///
/// [current_text 不 flush] 续跑时 state.current_text 保留累积状态——下一个
/// block 的 spacing 决策基于 "current_text 是否为空 / 末尾是否空行 / prev_was_list_item"，
/// 这些状态都是 spacing 正确决策的必要信息。最终输出时由调用方 clone + flush。
#[derive(Clone, Default, Debug)]
pub(crate) struct ConvertState {
    /// 已处理的 block 数（跳过 blocks[..processed_block_count]）。
    pub processed_block_count: usize,
    /// 上一个处理的 block 是否为 ListItem（用于连续 list item 之间不加空行的规则）。
    pub prev_was_list_item: bool,
    /// 累积缓冲区（多个 block 合并到一个 Text segment）。续跑时**不 flush**。
    pub current_text: Vec<Line<'static>>,
    /// 已 flush 的 segments（包含 Table + 已闭合的 Text）。
    pub segments: Vec<MarkdownSegment>,
    /// 已处理的 block 中是否包含 Table——用于缓存失效检查。
    /// Table 是动态块：后续追加行（数据行）会改变同一个 block 的内容，
    /// 而其他块（Paragraph/CodeBlock/ListItem）在闭合后内容不变。
    pub has_table_in_processed_blocks: bool,
}

/// 将 ratatui-kit-markdown 的 ParsedBlock 列表转换为 MarkdownSegment 序列。
///
/// 间距规则（统一）：
/// - 每个块级元素前加 **恰好一行**空行，除非：
///   (a) 是第一个有内容的块
///   (b) 是连续的列表项（列表项之间无空行）
/// - parser 生成的空 `Paragraph` 是列表分隔哨兵，跳过（间距由本函数统一管理）
///   `base_fg` 作为普通段落/列表项文本的兜底前景色（来自 `component.markdown.text`）。
pub(crate) fn convert_to_segments(
    blocks: &[ParsedBlock],
    theme: &MarkdownTheme,
    max_width: usize,
    base_fg: ratatui::style::Color,
) -> Vec<MarkdownSegment> {
    let mut state = ConvertState::default();
    let base_style = Style::default().fg(base_fg);
    convert_to_segments_with_state(blocks, theme, max_width, base_style, &mut state)
}

/// 与 `convert_to_segments` 同逻辑，但接受外部 `state` 以支持续跑。
///
/// [行为]
/// - 跳过 blocks[..state.processed_block_count]
/// - 处理 blocks[state.processed_block_count..]，更新 state
/// - 返回的 segments = state.segments clone + state.current_text flush
/// - **state.current_text 不 flush**（保留供下次续跑）
///
/// 调用方应在 sanitized text 以换行结尾时（最后一个 block 已闭合）把
/// state 持久化到 cache；否则 state 仅用于本次输出，不持久化。
pub(crate) fn convert_to_segments_with_state(
    blocks: &[ParsedBlock],
    theme: &MarkdownTheme,
    max_width: usize,
    base_style: Style,
    state: &mut ConvertState,
) -> Vec<MarkdownSegment> {
    for (i, block) in blocks.iter().enumerate() {
        if i < state.processed_block_count {
            continue;
        }

        // 跳过 parser 生成的空 Paragraph（列表前后的哨兵）
        if matches!(block, ParsedBlock::Paragraph(lines) if lines.is_empty()) {
            state.prev_was_list_item = false;
            state.processed_block_count = i + 1;
            continue;
        }

        let is_list_item = matches!(block, ParsedBlock::ListItem(_));

        // 分隔：非首块 + 非连续列表项 → 确保恰好一行空行
        if !(state.current_text.is_empty()
            || state
                .current_text
                .last()
                .is_some_and(|l| l.spans.is_empty())
            || is_list_item && state.prev_was_list_item)
        {
            state.current_text.push(Line::default());
        }

        match block {
            ParsedBlock::Heading(level, line) => {
                state.current_text.push(heading_line(level, line, theme));
            }
            ParsedBlock::Paragraph(para_lines) => {
                for line in para_lines {
                    state.current_text.push(style_line(line, theme, base_style));
                }
            }
            ParsedBlock::CodeBlock(lang, code_lines) => {
                state
                    .current_text
                    .extend(code_block_lines(lang, code_lines, theme));
            }
            ParsedBlock::ListItem(item) => {
                state
                    .current_text
                    .push(list_item_line(item, theme, base_style));
            }
            ParsedBlock::Rule => {
                let rule_char = "─".repeat(max_width.min(80));
                let rule_span = Span::styled(rule_char, theme.rule_style);
                state.current_text.push(Line::from(rule_span));
            }
            ParsedBlock::Table(headers, rows, alignments) => {
                // 表格前：冲刷已有文本为独立段
                trim_trailing_blanks(&mut state.current_text);
                if !state.current_text.is_empty() {
                    state.segments.push(MarkdownSegment::Text(std::mem::take(
                        &mut state.current_text,
                    )));
                }
                let col_widths =
                    compute_table_col_widths(headers, rows, alignments.len(), max_width);
                state.segments.push(MarkdownSegment::Table(TableData {
                    headers: headers.clone(),
                    rows: rows.clone(),
                    alignments: alignments.clone(),
                    col_widths,
                }));
                // Table 是动态块：后续追加行会改变同一 block 的内容。
                // 标记以便缓存层知道不能续跑此状态。
                state.has_table_in_processed_blocks = true;
            }
        }

        state.prev_was_list_item = is_list_item;
        state.processed_block_count = i + 1;
    }

    // 末尾 flush：clone state 并 flush current_text（state.current_text 不变）
    let mut final_segments = state.segments.clone();
    let mut final_text = state.current_text.clone();
    trim_trailing_blanks(&mut final_text);
    if !final_text.is_empty() {
        final_segments.push(MarkdownSegment::Text(final_text));
    }
    final_segments
}

/// 裁剪尾部空行。
fn trim_trailing_blanks(text: &mut Vec<Line<'static>>) {
    while text.last().is_some_and(|l| l.spans.is_empty()) {
        text.pop();
    }
}

/// 通用段落行渲染。`base_style` 作为普通文本的兜底前景色。
fn style_line(line: &Line<'static>, theme: &MarkdownTheme, base_style: Style) -> Line<'static> {
    Line::from(apply_span_styles(&line.spans, theme, Some(base_style)))
}
