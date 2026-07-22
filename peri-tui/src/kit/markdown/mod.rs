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
//! - `table`：compute_table_col_widths, table_data_to_lines (ratatui-kit 风格渲染)
//! - `convert`：convert_to_segments（块级分发）

mod code_block;
mod convert;
mod heading;
mod list;
mod span_style;
mod table;
pub mod types;

use ratatui::style::Color;
use ratatui_kit::{ComponentTheme, prelude::Palette};
use ratatui_kit_markdown::{MarkdownTheme, parse_markdown as rk_parse};

pub use table::table_data_to_lines;
pub use types::{MarkdownSegment, TableData};

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
    let parsed = rk_parse(&sanitized);
    let theme = MarkdownTheme::from_palette(&palette);
    convert::convert_to_segments(&parsed.blocks, &theme, max_width, base_fg)
}

/// 检测未闭合 fenced code block：逐行统计 ``` fence 数，奇数则末尾补一个闭合 fence。
/// 保守实现——不处理 indented code block、嵌套 fence、tilde fence (~~~) 等复杂场景。
/// 复杂场景由 ratatui-kit-markdown parser 的 finalize() 兜底。
fn ensure_closed_code_fences(input: &str) -> String {
    let fence_count = input
        .lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count();
    if fence_count % 2 == 1 {
        format!("{input}\n```")
    } else {
        input.to_string()
    }
}

// ── 增量缓存（Phase 2：文本字节级前缀比较）──────────────────────────────
//
// [Why] VM 级分片缓存解决了"哪些 VM 需要重渲染"的问题，但**单个流式 bubble 内部**
// 每个 token 仍触发整段 convert_to_segments。流式期间 text 末尾追加字符，
// 前面已闭合的 block（如已闭合的 ``` 代码块、已结束的 paragraph）内容完全不变。
//
// [契机] pulldown-cmark 是确定性解析器：相同文本前缀 → 相同 blocks 前缀。
// 因此只要检测 `text.starts_with(cache.stable_text)`，就能复用上次处理到
// `cache.stable_state.processed_block_count` 的累积状态，仅处理新增 block。
//
// [稳定前缀契约] cache.stable_text 必须以换行符结尾（\n 或 \n\n），保证其
// 对应的最后一个 block 已闭合——这是 pulldown-cmark 前缀一致性的必要条件。
// 调用方只在 sanitized text 以换行结尾时持久化 state。
//
// [spacing 正确性] convert_to_segments 的 spacing 决策依赖累积缓冲区尾部状态
// （current_text 是否为空 / 末尾是否空行 / prev_was_list_item），ConvertState
// 完整保留这些状态。续跑时新 block 的 spacing 决策与"全量重跑"完全一致。

/// 单个 markdown 渲染缓存（每个 AssistantBubble / UserBubble 一个）。
#[derive(Clone, Debug, Default)]
pub struct MarkdownRenderCache {
    /// 已稳定处理的文本前缀（必须以换行符结尾，保证最后一个 block 已闭合）。
    /// 空字符串表示缓存无效。
    stable_text: String,
    /// 上次处理 stable_text 时的 vis_width。
    stable_width: u16,
    /// 上次处理 stable_text 时的 palette。
    stable_palette: Palette,
    /// 上次处理 stable_text 后的累积状态（processed_block_count / current_text /
    /// segments / prev_was_list_item）。current_text 未 flush，保留累积状态供续跑。
    stable_state: convert::ConvertState,
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
    let parsed = rk_parse(&sanitized);
    let theme = MarkdownTheme::from_palette(&palette);
    let base_style = ratatui::style::Style::default().fg(base_fg);

    // 判断是否能复用 stable_state
    // [Table 缓存失效] Table 是动态块：追加行会改变同一 block 的内容（headers/rows），
    // 而其他块（Paragraph/CodeBlock/ListItem）闭合后不变。因此若已处理块中曾含 Table，
    // 必须全量重跑，否则旧 TableData（可能行数不足）会被复用。
    //
    // [表头翻转缓存失效] 流式期间 `| a | b |\n` 先被 pulldown-cmark 解析为 Paragraph，
    // 分隔符到达后同一文本前缀翻转为 Table。此时缓存的 processed_block_count 与旧
    // Paragraph block 绑定，但 block 类型已变——必须全量重跑，否则原始 pipe 格式永久残留。
    let can_reuse = cache.has_stable_prefix()
        && cache.stable_width == max_width as u16
        && cache.stable_palette == palette
        && sanitized.starts_with(&cache.stable_text)
        && cache.stable_state.processed_block_count <= parsed.blocks.len()
        && !cache.stable_state.has_table_in_processed_blocks
        && !cache.stable_state.has_potential_table_header;

    let mut state = if can_reuse {
        cache.stable_state.clone()
    } else {
        convert::ConvertState::default()
    };

    let segments = convert::convert_to_segments_with_state(
        &parsed.blocks,
        &theme,
        max_width,
        base_style,
        &mut state,
    );

    // 只在 sanitized 以换行符结尾时持久化（保证最后一个 block 已闭合）
    // —— 这是续跑正确性的契约：stable_text 对应的所有 block 在新 text 中
    // 仍保持完整。否则跳过持久化，下次 parse 仍可命中旧 stable_text（如果
    // sanitized 仍以 stable_text 为前缀）。
    if sanitized.ends_with('\n') {
        cache.stable_text = sanitized.clone();
        cache.stable_width = max_width as u16;
        cache.stable_palette = palette;
        cache.stable_state = state;
    }

    segments
}

// ── 测试 ────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
