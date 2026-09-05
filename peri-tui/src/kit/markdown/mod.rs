//! Markdown 解析（kit 路径专用）。
//!
//! 底层委托给 `ratatui_kit_markdown::parse_markdown`（公开 API），
//! 自行实现 `ParsedBlock` → `Line<'static>` 转换以适配 RENDER_CACHE 管线。
//! `ratatui_kit_markdown` 的 `RenderRow` / `render_rows_with_theme` 为
//! `pub(crate)`，外部不可用——此处复刻了 `style_spans` / `semantic_style`
//! 及块间距逻辑。
//!
//! 子模块组织：
//! - `types`：MarkdownSegment, TableData
//! - `span_style`：apply_span_styles, span_semantic_style
//! - `heading`：heading_line（不渲染 # 前缀）
//! - `list`：list_item_line
//! - `code_block`：highlight_code_block, code_block_lines, syntect 单例
//! - `table`：compute_table_col_widths（最长不可断词约束）、table_data_to_lines
//!   （智能断词换行 + unicode 网格线渲染）
//! - `convert`：convert_to_segments（块级分发）
//! - `scan`：图片前置扫描 + 占位替换（P0，T2）

mod code_block;
mod convert;
mod heading;
mod list;
mod scan;
mod span_style;
mod table;
pub mod types;

use std::borrow::Cow;
use std::sync::Arc;

use ratatui::style::Color;
use ratatui_kit::{ComponentTheme, prelude::Palette};
use ratatui_kit_markdown::{MarkdownTheme, ParsedBlock, parse_markdown as rk_parse};

pub use table::table_data_to_lines;
pub use types::{ImageSegment, MarkdownSegment, TableData};

// ── 公开 API ───────────────────────────────────────────────────────

/// 解析 markdown 为段落序列，表格作为独立 `Table` 段，不放 `Vec<Line>` 里。
/// `base_fg` 作为普通段落文本的前景色（来自主题 `component.markdown.text`）。
pub fn parse_markdown(
    input: &str,
    max_width: usize,
    palette: Palette,
    base_fg: Color,
) -> Vec<MarkdownSegment> {
    if input.is_empty() {
        return vec![];
    }
    // [防御] ratatui-kit-markdown parser 在 finalize() 已修复未闭合 ``` 代码块，
    // 但 peri-tui 仍保留兜底：流式期间偶发 fence 计数为奇数时，主动补一个闭合 fence，
    // 保证未闭合代码块内容不被丢弃。简单按行扫描，3+ backtick 开头记一次 fence。
    let sanitized = ensure_closed_code_fences(input);
    // [图片前置扫描] sanitize 之后、rk_parse 之前：`![alt](url)` → 占位 token
    // （S1 §4 管线硬性约束）。Options 与 rk_parse 逐位一致（scan::md_options，
    // S3 §3.5 漂移风险）；side table 供 T3 convert 查表（token → ImageInfo）。
    let (placeholder, image_infos) =
        scan::replace_images(&sanitized, &scan::scan_images(&sanitized));
    let parsed = rk_parse(&placeholder);
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(crate::kit::acp_bridge::PerfCounter::FullParse, 1);
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::FullParsedBytes,
            placeholder.len() as u64,
        );
    }
    let theme = MarkdownTheme::from_palette(&palette);
    let lookup = convert::image_lookup(&image_infos);
    let segments = convert::convert_to_segments(
        &parsed.blocks,
        &theme,
        max_width,
        base_fg,
        &image_infos,
        &lookup,
    );
    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::MaterializedLines,
        segments
            .iter()
            .map(|segment| match segment {
                MarkdownSegment::Text(lines) => lines.len(),
                MarkdownSegment::Table(table) => table.rows.len().saturating_add(1),
                MarkdownSegment::Image(image) => image.lines.len(),
            })
            .sum::<usize>() as u64,
    );
    segments
}

fn parse_markdown_piece(
    input: &str,
    max_width: usize,
    palette: Palette,
    base_fg: Color,
) -> (Vec<MarkdownSegment>, Vec<ParsedBlock>) {
    if input.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let sanitized = ensure_closed_code_fences(input);
    let (placeholder, image_infos) =
        scan::replace_images(&sanitized, &scan::scan_images(&sanitized));
    let parsed = rk_parse(&placeholder);
    let theme = MarkdownTheme::from_palette(&palette);
    let lookup = convert::image_lookup(&image_infos);
    let segments = convert::convert_to_segments(
        &parsed.blocks,
        &theme,
        max_width,
        base_fg,
        &image_infos,
        &lookup,
    );
    (segments, parsed.blocks)
}

fn convert_parsed_piece(
    blocks: &[ParsedBlock],
    max_width: usize,
    palette: Palette,
    base_fg: Color,
) -> Vec<MarkdownSegment> {
    let theme = MarkdownTheme::from_palette(&palette);
    convert::convert_to_segments(blocks, &theme, max_width, base_fg, &[], &Default::default())
}

fn stable_chunk_end(input: &str, start: usize) -> usize {
    let mut end = start;
    let mut cursor = start;
    while let Some(relative) = input[cursor..].find("\n\n") {
        let candidate_end = cursor + relative + 2;
        let candidate = &input[end..candidate_end];
        // References can retroactively resolve image/link syntax. Keep such regions mutable.
        // Fences are frozen only when balanced inside the candidate.
        let fences = candidate
            .lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count();
        let table_like = candidate.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with('|') && trimmed.matches('|').count() >= 2
        });
        let list_like = candidate.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- ")
                || trimmed.starts_with("* ")
                || trimmed.starts_with("+ ")
                || trimmed
                    .split_once(". ")
                    .is_some_and(|(number, _)| number.chars().all(|c| c.is_ascii_digit()))
        });
        if candidate.contains("![")
            || (candidate.contains('[') && candidate.contains(']'))
            || fences % 2 == 1
            || table_like
            || list_like
        {
            break;
        }
        end = candidate_end;
        cursor = candidate_end;
    }
    end
}

/// Phase C：复用 immutable rendered chunks，仅 preprocess/parse/materialize 保守 mutable tail。
pub fn parse_markdown_chunks_cached(
    input: &str,
    max_width: usize,
    palette: Palette,
    base_fg: Color,
    cache: &mut MarkdownRenderCache,
) -> RenderedMarkdown {
    if input.is_empty() {
        *cache = MarkdownRenderCache::default();
        return RenderedMarkdown::default();
    }

    let append_only = input.starts_with(cache.chunk_source.as_str());
    let reusable =
        append_only && cache.chunk_width == max_width as u16 && cache.chunk_palette == palette;
    if append_only && !reusable && !cache.stable_chunk_blocks.is_empty() {
        cache.stable_chunks = cache
            .stable_chunk_blocks
            .iter()
            .map(|blocks| Arc::new(convert_parsed_piece(blocks, max_width, palette, base_fg)))
            .collect();
    } else if !append_only {
        cache.chunk_source.clear();
        cache.stable_source_end = 0;
        cache.stable_chunk_blocks.clear();
        cache.stable_chunks.clear();
    }

    let next_end = stable_chunk_end(input, cache.stable_source_end);
    if next_end > cache.stable_source_end {
        let chunk_source = &input[cache.stable_source_end..next_end];
        let (chunk, blocks) = parse_markdown_piece(chunk_source, max_width, palette, base_fg);
        #[cfg(test)]
        {
            crate::kit::acp_bridge::observe_perf(crate::kit::acp_bridge::PerfCounter::TailParse, 1);
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::TailParsedBytes,
                (next_end - cache.stable_source_end) as u64,
            );
            crate::kit::acp_bridge::observe_perf(
                crate::kit::acp_bridge::PerfCounter::MaterializedLines,
                segment_line_count(&chunk),
            );
        }
        cache.stable_chunk_blocks.push(blocks);
        cache.stable_chunks.push(Arc::new(chunk));
        cache.stable_source_end = next_end;
    }

    let tail_input = &input[cache.stable_source_end..];
    let (tail, _) = parse_markdown_piece(tail_input, max_width, palette, base_fg);
    #[cfg(test)]
    if !tail_input.is_empty() {
        crate::kit::acp_bridge::observe_perf(crate::kit::acp_bridge::PerfCounter::TailParse, 1);
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::TailParsedBytes,
            tail_input.len() as u64,
        );
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::MaterializedLines,
            segment_line_count(&tail),
        );
    }

    if cache.chunk_source.len() < cache.stable_source_end {
        cache
            .chunk_source
            .push_str(&input[cache.chunk_source.len()..cache.stable_source_end]);
    }
    cache.chunk_width = max_width as u16;
    cache.chunk_palette = palette;
    RenderedMarkdown {
        stable: cache.stable_chunks.clone(),
        tail,
    }
}

/// Terminal/freeze correctness barrier：完整输入的一次性 full parse 作为最终结果。
pub fn parse_markdown_terminal(
    input: &str,
    max_width: usize,
    palette: Palette,
    base_fg: Color,
    cache: &mut MarkdownRenderCache,
) -> RenderedMarkdown {
    let full = parse_markdown(input, max_width, palette, base_fg);
    cache.chunk_source.clear();
    cache.chunk_source.push_str(input);
    cache.chunk_width = max_width as u16;
    cache.chunk_palette = palette;
    cache.stable_source_end = 0;
    cache.stable_chunk_blocks.clear();
    cache.stable_chunks.clear();
    RenderedMarkdown {
        stable: Vec::new(),
        tail: full,
    }
}

#[cfg(test)]
fn segment_line_count(segments: &[MarkdownSegment]) -> u64 {
    segments
        .iter()
        .map(|segment| match segment {
            MarkdownSegment::Text(lines) => lines.len(),
            MarkdownSegment::Table(table) => table.rows.len().saturating_add(1),
            MarkdownSegment::Image(image) => image.lines.len(),
        })
        .sum::<usize>() as u64
}

/// 检测未闭合 fenced code block：逐行统计 ``` fence 数，奇数则末尾补一个闭合 fence。
/// 返回 `Cow`：fence 数为偶数（常见路径）时零拷贝借用输入，避免每次流式 token
/// 都复制整段文本（perf：消除每 token 一份 O(N) 拷贝）。
/// 保守实现——不处理 indented code block、嵌套 fence、tilde fence (~~~) 等复杂场景。
/// 复杂场景由 ratatui-kit-markdown parser 的 finalize() 兜底。
fn ensure_closed_code_fences(input: &str) -> Cow<'_, str> {
    let fence_count = input
        .lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count();
    if fence_count % 2 == 1 {
        Cow::Owned(format!("{input}\n```"))
    } else {
        Cow::Borrowed(input)
    }
}

// ── 增量缓存（Phase 2：文本字节级前缀比较）──────────────────────────────
//
// [Why] VM 级分片缓存解决了"哪些 VM 需要重渲染"的问题，但**单个流式 bubble 内部**
// 每个 token 仍触发整段 convert_to_segments。流式期间 text 末尾追加字符，
// 前面已闭合的 block（如已闭合的 ``` 代码块、已结束的 paragraph）内容完全不变。
//
// [契机] pulldown-cmark 是确定性解析器：相同文本前缀 → 相同 blocks 前缀（流式
// 追加在末尾，只能影响尾部块）。因此只要检测
// `placeholder.starts_with(cache.stable_text)`（placeholder = sanitize + 图片
// 占位替换后的 rk_parse 输入，S1 §2.3：缓存键必须是替换后文本，否则图片
// 闭合瞬间会静默复用旧 blocks），就能复用上次处理到
// `cache.stable_state.processed_block_count` 的累积状态，仅处理新增 block。
//
// [稳定前缀契约] 持久化（persist）前必须回滚「尾部不稳定块」
// （convert::rollback_trailing_unstable）：
//   1. 尾部连续空段落（列表哨兵，追加后移位/消失）——回滚
//   2. 最后一个非空块：placeholder 不以空行（\n\n）结尾时回滚（段落同行/soft-break
//      增长、列表项 lazy continuation、标题同行增长、缩进/未闭合代码块行数增长、
//      `---`→`---x` 类型翻转、表头翻转的尾块场景）
// 回滚后 processed_block_count 停在「稳定边界」处，续跑时重新处理尾部块——
// 即使追加改变了尾部块的内容/类型，重渲结果也与全量解析一致。
// 更早的块由空行/块级边界保证稳定；表头翻转的中间块场景由
// `has_potential_table_header` 失效，表格行增长由 `has_table_in_processed_blocks` 失效。
//
// [性能] 相比早期实现（仅 sanitized 以 \n 结尾时持久化），任意输入都可持久化：
// 流式散文（段落内无换行）不再每 token 全量 convert。缓存命中路径零深拷贝：
// stable_state 用 mem::take 移出（不 clone），stable_text 用 strip_prefix + push_str
// 增量维护（O(delta) 而非 O(N) 拷贝）。
//
// [spacing 正确性] convert_to_segments 的 spacing 决策依赖累积缓冲区尾部状态
// （current_text 是否为空 / 末尾是否空行 / prev_was_list_item），ConvertState
// 完整保留这些状态。续跑时新 block 的 spacing 决策与"全量重跑"完全一致。

/// 已渲染 Markdown 的稳定前缀与保守可变尾部。
///
/// stable chunks 使用 `Arc` 保持跨 publication 的 identity；只有 `tail` 会在追加时重建。
#[derive(Clone, Debug, Default)]
pub struct RenderedMarkdown {
    pub stable: Vec<Arc<Vec<MarkdownSegment>>>,
    pub tail: Vec<MarkdownSegment>,
}

impl RenderedMarkdown {
    pub fn segments(&self) -> impl Iterator<Item = &MarkdownSegment> {
        self.stable
            .iter()
            .flat_map(|chunk| chunk.iter())
            .chain(&self.tail)
    }

    #[cfg(test)]
    pub(crate) fn stable_identities(&self) -> Vec<*const Vec<MarkdownSegment>> {
        self.stable.iter().map(Arc::as_ptr).collect()
    }
}

/// 单个 markdown 渲染缓存（每个 AssistantBubble / UserBubble 一个）。
#[derive(Clone, Debug, Default)]
pub struct MarkdownRenderCache {
    /// 旧增量 convert 路径的稳定 parser 输入。
    stable_text: String,
    stable_width: u16,
    stable_palette: Palette,
    stable_state: convert::ConvertState,
    /// Phase C：只在明确空行边界冻结的 rendered chunks。
    chunk_source: String,
    chunk_width: u16,
    chunk_palette: Palette,
    stable_source_end: usize,
    stable_chunk_blocks: Vec<Vec<ratatui_kit_markdown::ParsedBlock>>,
    stable_chunks: Vec<Arc<Vec<MarkdownSegment>>>,
}

impl MarkdownRenderCache {
    /// 是否有有效的稳定前缀（可复用）。
    fn has_stable_prefix(&self) -> bool {
        !self.stable_text.is_empty()
    }

    /// 测试辅助：当前 stable_text 长度。0 表示缓存空。
    #[cfg(test)]
    pub(crate) fn stable_text_len(&self) -> usize {
        self.stable_text.len()
    }

    /// 测试辅助：当前 stable_state 中已处理的 block 数。
    #[cfg(test)]
    pub(crate) fn stable_processed_block_count(&self) -> usize {
        self.stable_state.processed_block_count
    }

    #[cfg(test)]
    pub(crate) fn stable_chunk_count(&self) -> usize {
        self.stable_chunks.len()
    }

    #[cfg(test)]
    pub(crate) fn stable_parsed_blocks(&self) -> usize {
        self.stable_chunk_blocks.iter().map(Vec::len).sum()
    }
}

/// 带缓存的 parse_markdown：命中稳定前缀时仅处理新增 block，否则全量重跑。
///
/// 调用方应将 cache 与 VM（AssistantBubble）一一绑定，避免跨 VM 复用。
/// 在 message_area/mod.rs::VmCacheSlot 中嵌入。
/// `base_fg` 作为普通段落文本的前景色（来自主题 `component.markdown.text`）。
pub fn parse_markdown_cached(
    input: &str,
    max_width: usize,
    palette: Palette,
    base_fg: Color,
    cache: &mut MarkdownRenderCache,
) -> Vec<MarkdownSegment> {
    if input.is_empty() {
        return vec![];
    }
    let sanitized = ensure_closed_code_fences(input);
    // [图片前置扫描] 同 parse_markdown：sanitize 之后、rk_parse 之前替换为占位
    // token。占位替换后的文本是 rk_parse 输入，**也是缓存键**（S1 §2.3：
    // 占位替换必须参与缓存键，否则图片闭合瞬间 placeholder 前缀断裂时会
    // 静默复用旧 blocks——正确性硬要求，不是优化）。side table 供 T3 convert
    // 查表（token → ImageInfo）。
    let (placeholder, image_infos) =
        scan::replace_images(&sanitized, &scan::scan_images(&sanitized));
    let parsed = rk_parse(&placeholder);
    let theme = MarkdownTheme::from_palette(&palette);
    let base_style = ratatui::style::Style::default().fg(base_fg);
    let lookup = convert::image_lookup(&image_infos);

    // 判断是否能复用 stable_state
    // [Table 缓存失效] Table 是动态块：追加行会改变同一 block 的内容（headers/rows），
    // 而其他块（Paragraph/CodeBlock/ListItem）闭合后不变。因此若已处理块中曾含 Table，
    // 必须全量重跑，否则旧 TableData（可能行数不足）会被复用。
    //
    // [表头翻转缓存失效] 流式期间 `| a | b |\n` 先被 pulldown-cmark 解析为 Paragraph，
    // 分隔符到达后同一文本前缀翻转为 Table（中间块场景；尾块场景由 persist 前回滚覆盖）。
    // 此时缓存的 processed_block_count 与旧 Paragraph block 绑定，但 block 类型已变——
    // 必须全量重跑，否则原始 pipe 格式永久残留。
    let prefix_delta = placeholder.strip_prefix(&cache.stable_text);
    let can_reuse = cache.has_stable_prefix()
        && cache.stable_width == max_width as u16
        && cache.stable_palette == palette
        && prefix_delta.is_some()
        && cache.stable_state.processed_block_count <= parsed.blocks.len()
        && !cache.stable_state.has_table_in_processed_blocks
        && !cache.stable_state.has_potential_table_header;
    #[cfg(test)]
    {
        let (count, bytes) = if can_reuse {
            (
                crate::kit::acp_bridge::PerfCounter::TailParse,
                prefix_delta.unwrap().len(),
            )
        } else {
            (
                crate::kit::acp_bridge::PerfCounter::FullParse,
                placeholder.len(),
            )
        };
        let byte_counter = if can_reuse {
            crate::kit::acp_bridge::PerfCounter::TailParsedBytes
        } else {
            crate::kit::acp_bridge::PerfCounter::FullParsedBytes
        };
        crate::kit::acp_bridge::observe_perf(count, 1);
        crate::kit::acp_bridge::observe_perf(byte_counter, bytes as u64);
    }

    // [perf] can_reuse 路径用 mem::take 把累积状态移出缓存（零深拷贝），
    // 处理完再写回——替代早期实现的 cache.stable_state.clone()（每 token 一份
    // 全量 Vec<Line> 深拷贝）。
    let mut state = if can_reuse {
        std::mem::take(&mut cache.stable_state)
    } else {
        convert::ConvertState::default()
    };

    let segments = convert::convert_to_segments_with_state(
        &parsed.blocks,
        &theme,
        max_width,
        base_style,
        &mut state,
        &image_infos,
        &lookup,
    );
    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::MaterializedLines,
        segments
            .iter()
            .map(|segment| match segment {
                MarkdownSegment::Text(lines) => lines.len(),
                MarkdownSegment::Table(table) => table.rows.len().saturating_add(1),
                MarkdownSegment::Image(image) => image.lines.len(),
            })
            .sum::<usize>() as u64,
    );

    // 持久化：先回滚「尾部不稳定块」，再写回缓存。
    // [Why 放宽] 早期实现只在 sanitized 以换行符结尾时持久化，流式散文（段落内
    // 无换行）时 stable_text 恒空 → 每 token 全量解析 + 全量 convert（最坏 O(N²) 累积）。
    // 现在任意输入都持久化：回滚保证续跑正确性，散文场景也能命中缓存增量续跑。
    // [回归修复] 尾部空段落（列表哨兵）由 rollback 统一回滚——替代早期实现的
    // trailing_empties 剔除（只处理尾部空段，且需要手动修正 prev_was_list_item）。
    // 空行判定基于 placeholder：占位 token 单行无换行，与 sanitized 判定等价
    // （S1 §4）。
    convert::rollback_trailing_unstable(&parsed.blocks, &mut state, placeholder.ends_with("\n\n"));
    // [T3 图片缓存安全] 已处理块含图片时回滚全部增量状态：图片拆段 flush 出
    // current_text（Text/Image 段），续跑无法重建已 flush 段 → 含图片文本不
    // 参与增量续跑，每帧全量重跑（S1 §6.2：图片语法出现率低，O(N) 可接受；
    // §8.1 R3 正确性优先）。无图片消息不受影响（NUL 预检快速路径）。
    convert::rollback_image_blocks(&parsed.blocks, &mut state, &lookup);
    if let Some(delta) = prefix_delta {
        // 文本前缀未变（含宽度/主题变化但文本相同）：stable_text 增量扩展，
        // 摊销 O(delta) 而非每 token O(N) 拷贝。
        cache.stable_text.push_str(delta);
    } else {
        cache.stable_text = placeholder;
    }
    cache.stable_width = max_width as u16;
    cache.stable_palette = palette;
    cache.stable_state = state;

    segments
}

// ── 测试 ────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
