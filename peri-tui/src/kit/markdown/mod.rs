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

mod code_block;
mod convert;
mod heading;
mod list;
mod span_style;
mod table;
pub mod types;

use std::borrow::Cow;

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
// 追加在末尾，只能影响尾部块）。因此只要检测 `sanitized.starts_with(cache.stable_text)`，
// 就能复用上次处理到 `cache.stable_state.processed_block_count` 的累积状态，仅处理新增 block。
//
// [稳定前缀契约] 持久化（persist）前必须回滚「尾部不稳定块」
// （convert::rollback_trailing_unstable）：
//   1. 尾部连续空段落（列表哨兵，追加后移位/消失）——回滚
//   2. 最后一个非空块：sanitized 不以空行（\n\n）结尾时回滚（段落同行/soft-break
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

/// 单个 markdown 渲染缓存（每个 AssistantBubble / UserBubble 一个）。
#[derive(Clone, Debug, Default)]
pub struct MarkdownRenderCache {
    /// 已稳定处理的文本前缀（上次 persist 的 sanitized text，可为任意结尾——
    /// 正确性由 persist 前回滚尾部不稳定块保证）。
    /// 空字符串表示缓存无效。
    stable_text: String,
    /// 上次处理 stable_text 时的 vis_width。
    stable_width: u16,
    /// 上次处理 stable_text 时的 palette。
    stable_palette: Palette,
    /// 上次处理 stable_text 后的累积状态（processed_block_count / current_text /
    /// prev_was_list_item / block_line_ends）。current_text 未 flush，
    /// 保留累积状态供续跑；segments 返回时被 take 清空，不在此常驻。
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
    // 分隔符到达后同一文本前缀翻转为 Table（中间块场景；尾块场景由 persist 前回滚覆盖）。
    // 此时缓存的 processed_block_count 与旧 Paragraph block 绑定，但 block 类型已变——
    // 必须全量重跑，否则原始 pipe 格式永久残留。
    let prefix_delta = sanitized.strip_prefix(&cache.stable_text);
    let can_reuse = cache.has_stable_prefix()
        && cache.stable_width == max_width as u16
        && cache.stable_palette == palette
        && prefix_delta.is_some()
        && cache.stable_state.processed_block_count <= parsed.blocks.len()
        && !cache.stable_state.has_table_in_processed_blocks
        && !cache.stable_state.has_potential_table_header;

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
    );

    // 持久化：先回滚「尾部不稳定块」，再写回缓存。
    // [Why 放宽] 早期实现只在 sanitized 以换行符结尾时持久化，流式散文（段落内
    // 无换行）时 stable_text 恒空 → 每 token 全量解析 + 全量 convert（最坏 O(N²) 累积）。
    // 现在任意输入都持久化：回滚保证续跑正确性，散文场景也能命中缓存增量续跑。
    // [回归修复] 尾部空段落（列表哨兵）由 rollback 统一回滚——替代早期实现的
    // trailing_empties 剔除（只处理尾部空段，且需要手动修正 prev_was_list_item）。
    convert::rollback_trailing_unstable(&parsed.blocks, &mut state, sanitized.ends_with("\n\n"));
    if let Some(delta) = prefix_delta {
        // 文本前缀未变（含宽度/主题变化但文本相同）：stable_text 增量扩展，
        // 摊销 O(delta) 而非每 token O(N) 拷贝。
        cache.stable_text.push_str(delta);
    } else {
        cache.stable_text = sanitized.into_owned();
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
