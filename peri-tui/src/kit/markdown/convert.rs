use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_kit_markdown::{MarkdownTheme, ParsedBlock};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
/// 这通过 `text.starts_with(cache.stable_text)` + 持久化前回滚「尾部不稳定块」
/// （见 [`rollback_trailing_unstable`]）来保证。
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
    /// 返回时被 `mem::take` 清空，不跨调用保留：无 Table 时恒为空（文本全部
    /// 累积在 current_text）；有 Table 时 `has_table_in_processed_blocks` 使
    /// 缓存失效、每次全量重跑。续跑循环从不读取本字段。
    pub segments: Vec<MarkdownSegment>,
    /// 每个已处理 block 处理完时 current_text 的行数边界。
    /// `block_line_ends[i]` = 处理完 blocks[i] 后 current_text.len()，长度恒等于
    /// processed_block_count。回滚尾部不稳定块时用于截断 current_text。
    pub block_line_ends: Vec<usize>,
    /// 已处理的 block 中是否包含 Table——用于缓存失效检查。
    /// Table 是动态块：后续追加行（数据行）会改变同一个 block 的内容，
    /// 而其他块（Paragraph/CodeBlock/ListItem）在闭合后内容不变。
    pub has_table_in_processed_blocks: bool,
    /// 已处理的 block 中是否包含「可能是表头行」的 Paragraph——用于缓存失效检查。
    /// 流式场景下，`| a | b |\n` 先到达时 pulldown-cmark 解析为 Paragraph，
    /// 分隔符到达后同一前缀翻转为 Table。此时缓存必须失效，否则原始 pipe 格式
    /// 会永远停留在输出中。
    pub has_potential_table_header: bool,
}

/// 持久化前回滚「尾部不稳定块」，使续跑契约成立。
///
/// 流式追加只发生在文本末尾，因此**只有尾部块可能变化**；更早的块由空行/块级
/// 边界保证稳定（表头翻转由 `has_potential_table_header` 单独失效，表格增长由
/// `has_table_in_processed_blocks` 单独失效）。回滚两个部分：
///
/// 1. **尾部连续空段落**（列表前后插入的哨兵，追加后移位/消失，如
///    `[Empty, LI, Empty]` → `[Empty, LI, LI, Empty]`）——一律回滚。
/// 2. **最后一个非空块**：当 sanitized 不以空行（`\n\n`）结尾时，它可能随追加
///    而改变内容，必须回滚让续跑重渲：
///    - Paragraph：同行/soft-break 增长（`para` → `para more`）
///    - ListItem：lazy continuation（`- A\n` → `- A\nmore`）
///    - Heading：同行增长（`# h` → `# h x`）
///    - CodeBlock：缩进代码块/未闭合 fence 行数增长（`    a` → `    a\n    b`）
///    - Rule：类型翻转（`---` → `---x`）
///
/// 以 `\n\n` 结尾时最后一个非空块已被空行闭合：追加内容只能产生新块，旧块内容
/// 不变（已验证：`para\n\n` + `more` → `[P(para), P(more)]`）。
///
/// [代价权衡] 对「已闭合但未以空行（`\n\n`）结尾的块」——闭合代码块（```` ```\n ````）、
/// 列表项、段落等——本函数每次追加都会回滚其进度，续跑时重新处理该块：该场景的
/// convert 成本与旧实现最坏情况持平（旧实现本就不持久化该输入，或按 `\n` 结尾
/// 持久化后仍需 O(delta) 重处理）。这是正确性优先的取舍而非缺陷——散文（段落内
/// 追加、无闭合块）与以空行闭合的块仍命中增量续跑（O(delta)），正确性由
/// `mod_test.rs` 的回归测试保障。若未来要优化该场景，需先证明块在追加下不变
/// （如 fenced code block 以 `\n` 结尾时行可增长，不能仅按块类型判定稳定）。
pub(crate) fn rollback_trailing_unstable(
    blocks: &[ParsedBlock],
    state: &mut ConvertState,
    text_ends_with_blank_line: bool,
) {
    let mut n = state.processed_block_count;
    // 1. 尾部连续空段落（列表哨兵）全部回滚
    while n > 0 && matches!(&blocks[n - 1], ParsedBlock::Paragraph(lines) if lines.is_empty()) {
        n -= 1;
    }
    // 2. 未闭合的最后一个非空块回滚
    if n > 0 && !text_ends_with_blank_line {
        n -= 1;
    }
    if n < state.processed_block_count {
        let keep = if n == 0 {
            0
        } else {
            state.block_line_ends[n - 1]
        };
        state.current_text.truncate(keep);
        state.processed_block_count = n;
        state.prev_was_list_item = n > 0 && matches!(&blocks[n - 1], ParsedBlock::ListItem(_));
        state.block_line_ends.truncate(n);
    }
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
/// - 返回的 segments = state.segments（mem::take 移出） + state.current_text flush
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
            state.block_line_ends.push(state.current_text.len());
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
                state.current_text.extend(wrap_styled_line(
                    &heading_line(level, line, theme),
                    max_width,
                ));
            }
            ParsedBlock::Paragraph(para_lines) => {
                // 检测「可能是表头行」的 Paragraph：首行以 | 开头，
                // 流式期间分隔符未到达时被误判为 Paragraph，后续 block 类型会翻转为 Table。
                if !state.has_potential_table_header
                    && para_lines
                        .first()
                        .and_then(|l| l.spans.first())
                        .is_some_and(|s| s.content.starts_with('|'))
                {
                    state.has_potential_table_header = true;
                }
                for line in para_lines {
                    state.current_text.extend(wrap_styled_line(
                        &style_line(line, theme, base_style),
                        max_width,
                    ));
                }
            }
            ParsedBlock::CodeBlock(lang, code_lines) => {
                for line in code_block_lines(lang, code_lines, theme) {
                    state
                        .current_text
                        .extend(wrap_styled_line(&line, max_width));
                }
            }
            ParsedBlock::ListItem(item) => {
                state.current_text.extend(wrap_styled_line(
                    &list_item_line(item, theme, base_style),
                    max_width,
                ));
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
        state.block_line_ends.push(state.current_text.len());
    }

    // 末尾 flush：segments 用 mem::take 移出（零拷贝）——续跑路径下 segments
    // 恒为空（无 Table 时全部文本都累积在 current_text；有 Table 时
    // has_table_in_processed_blocks 令缓存失效、state 每次从 default 重建），
    // 因此不会丢失跨调用段。current_text 必须 clone：其内容同时服务
    // 返回全量输出与续跑 spacing 决策（保留在 state 中，见 struct doc）。
    let mut final_segments = std::mem::take(&mut state.segments);
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

/// 按 display width 将带样式行折为多行（保留 span 样式）。
///
/// [Why] 渲染层（消息区 Paragraph）只在**视口宽度**处 wrap——convert 阶段不折行时，
/// 超宽 md 行（长段落/标题/列表项/代码行）会被二次折行，折出的行丢失 `│` 竖线前缀
/// （左侧竖线被打断）。在 convert 阶段折行后，`prefixed_cont_line` 会给每个折出行
/// 统一套前缀，竖线保持连续，且每行宽 ≤ max_width（前缀 + 内容 ≤ 视口，不触发二次折行）。
///
/// 度量口径与 §12 一致：按 grapheme cluster + display width（CJK 双宽 / emoji ZWJ /
/// combining mark 不被从中间切开），同 [`crate::truncate::wrap_by_width`]；超宽单词
/// 按宽度切分，**不丢内容**。`max_width == 0`（极端窄屏）时原样返回单行。
pub(super) fn wrap_styled_line(line: &Line<'static>, max_width: usize) -> Vec<Line<'static>> {
    if max_width == 0 || line.width() <= max_width {
        return vec![line.clone()];
    }
    let mut rows: Vec<Vec<(String, Style)>> = Vec::new();
    let mut cur: Vec<(String, Style)> = Vec::new();
    let mut cur_width = 0usize;
    for span in &line.spans {
        for g in span.content.graphemes(true) {
            let w = g.width();
            if !cur.is_empty() && cur_width + w > max_width {
                rows.push(std::mem::take(&mut cur));
                cur_width = 0;
            }
            // 单个 grapheme 即超宽（罕见）：独占一行也不丢内容
            if w > max_width && cur.is_empty() {
                rows.push(vec![(g.to_string(), span.style)]);
                continue;
            }
            cur.push((g.to_string(), span.style));
            cur_width += w;
        }
    }
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur);
    }
    rows.into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|(content, style)| Span::styled(content, style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}
