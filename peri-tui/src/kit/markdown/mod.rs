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

use ratatui_kit::{ComponentTheme, prelude::Palette};
use ratatui_kit_markdown::{MarkdownTheme, parse_markdown as rk_parse};

pub use table::table_data_to_lines;
pub use types::{MarkdownSegment, TableData};

// ── 公开 API ───────────────────────────────────────────────────────

/// 解析 markdown 为段落序列，表格作为独立 `Table` 段，不放 `Vec<Line>` 里。
pub fn parse_markdown(input: &str, max_width: usize, palette: Palette) -> Vec<MarkdownSegment> {
    if input.is_empty() {
        return vec![];
    }
    // [防御] ratatui-kit-markdown parser 在 finalize() 已修复未闭合 ``` 代码块，
    // 但 peri-tui 仍保留兜底：流式期间偶发 fence 计数为奇数时，主动补一个闭合 fence，
    // 保证未闭合代码块内容不被丢弃。简单按行扫描，3+ backtick 开头记一次 fence。
    let sanitized = ensure_closed_code_fences(input);
    let parsed = rk_parse(&sanitized);
    let theme = MarkdownTheme::from_palette(&palette);
    convert::convert_to_segments(&parsed.blocks, &theme, max_width)
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
#[derive(Clone, Debug)]
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

impl Default for MarkdownRenderCache {
    fn default() -> Self {
        Self {
            stable_text: String::new(),
            stable_width: 0,
            stable_palette: Palette::default(),
            stable_state: convert::ConvertState::default(),
        }
    }
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
pub fn parse_markdown_cached(
    input: &str,
    max_width: usize,
    palette: Palette,
    cache: &mut MarkdownRenderCache,
) -> Vec<MarkdownSegment> {
    if input.is_empty() {
        return vec![];
    }
    let sanitized = ensure_closed_code_fences(input);
    let parsed = rk_parse(&sanitized);
    let theme = MarkdownTheme::from_palette(&palette);

    // 判断是否能复用 stable_state
    let can_reuse = cache.has_stable_prefix()
        && cache.stable_width == max_width as u16
        && cache.stable_palette == palette
        && sanitized.starts_with(&cache.stable_text)
        && cache.stable_state.processed_block_count <= parsed.blocks.len();

    let mut state = if can_reuse {
        cache.stable_state.clone()
    } else {
        convert::ConvertState::default()
    };

    let segments =
        convert::convert_to_segments_with_state(&parsed.blocks, &theme, max_width, &mut state);

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
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    /// 测试辅助：将 parse_markdown 返回的段落展平为 Line 列表。
    fn flatten(segments: &[MarkdownSegment]) -> Vec<ratatui::text::Line<'static>> {
        segments
            .iter()
            .flat_map(|s| match s {
                MarkdownSegment::Text(lines) => lines.clone(),
                MarkdownSegment::Table(_) => vec![],
            })
            .collect()
    }

    #[test]
    fn test_empty_input() {
        let result = flatten(&parse_markdown("", 80, Palette::default()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_heading() {
        let result = flatten(&parse_markdown("# Hello", 80, Palette::default()));
        assert_eq!(result.len(), 1);
        let line = &result[0];
        // 不渲染 # 前缀，标题文本当普通段落
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "Hello");
    }

    #[test]
    fn test_paragraph() {
        let result = flatten(&parse_markdown("hello world", 80, Palette::default()));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].spans[0].content, "hello world");
    }

    #[test]
    fn test_adjacent_paragraphs() {
        let result = flatten(&parse_markdown("a\n\nb", 80, Palette::default()));
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].spans[0].content, "a");
        assert!(result[1].spans.is_empty());
        assert_eq!(result[2].spans[0].content, "b");
    }

    #[test]
    fn test_inline_code() {
        let result = flatten(&parse_markdown("use `code` here", 80, Palette::default()));
        let line = &result[0];
        // 行内代码不含 backtick 标记，使用 inline_code_style 颜色
        let code_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "code")
            .expect("inline code span should not contain backtick markers");
        // Palette::default().info = Color::Blue
        assert_eq!(
            code_span.style.fg,
            Some(ratatui::style::Color::Blue),
            "inline code should use palette.info color"
        );
        // 不应有背景色
        assert_eq!(
            code_span.style.bg, None,
            "inline code should not have background"
        );
    }

    #[test]
    fn test_unordered_list() {
        let result = flatten(&parse_markdown(
            "- item 1\n- item 2",
            80,
            Palette::default(),
        ));
        let non_empty: Vec<_> = result.iter().filter(|l| !l.spans.is_empty()).collect();
        assert_eq!(non_empty.len(), 2, "expected 2 non-empty list item lines");
        assert!(
            non_empty[0]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "• ")
        );
        assert!(
            non_empty[1]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "• ")
        );
    }

    #[test]
    fn test_code_block() {
        let result = flatten(&parse_markdown(
            "```rust\nlet x = 1;\n```",
            80,
            Palette::default(),
        ));
        // 单一代码块：至少渲染一行代码
        assert!(!result.is_empty());
    }

    #[test]
    fn test_code_block_spacing() {
        let result = flatten(&parse_markdown(
            "text\n\n```rust\nlet x = 1;\n```",
            80,
            Palette::default(),
        ));
        // 段落 + 空行分隔 + 代码行
        assert!(result.len() >= 3);
        // 第一行是段落文本
        assert_eq!(result[0].spans[0].content, "text");
        // 第二行是空行（分隔）
        assert!(result[1].spans.is_empty());
    }

    #[test]
    fn test_rule() {
        let result = flatten(&parse_markdown("---", 80, Palette::default()));
        assert_eq!(result.len(), 1);
        let content: String = result[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains('─'));
    }

    #[test]
    fn test_bold_text() {
        let result = flatten(&parse_markdown("**bold**", 80, Palette::default()));
        let line = &result[0];
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "bold text should have BOLD modifier"
        );
    }

    // ── 未闭合 code block 修复（步骤 1）──

    #[test]
    fn test_unclosed_code_block_content_visible() {
        // [回归测试] 流式输入末尾若 ``` 未闭合，内容不应被丢弃
        let input = "```rust\nlet x = 1;\nlet y = 2;";
        let result = flatten(&parse_markdown(input, 80, Palette::default()));
        let content: String = result
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref().to_string())
            .collect();
        assert!(
            content.contains("let x = 1;"),
            "未闭合代码块首行内容应可见，实际：{content:?}"
        );
        assert!(
            content.contains("let y = 2;"),
            "未闭合代码块次行内容应可见，实际：{content:?}"
        );
    }

    #[test]
    fn test_closed_code_block_unchanged_after_fix() {
        // 闭合的代码块渲染结果稳定（验证修复不引入回归）
        let input = "```rust\nlet x = 1;\n```";
        let result = flatten(&parse_markdown(input, 80, Palette::default()));
        let content: String = result
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref().to_string())
            .collect();
        assert!(
            content.contains("let x = 1;"),
            "闭合代码块内容应可见，实际：{content:?}"
        );
    }

    // ── ensure_closed_code_fences ──

    #[test]
    fn test_ensure_closed_code_fences_even_count_unchanged() {
        // 已闭合：fence 数为偶数，不补
        let input = "```rust\ncode\n```";
        assert_eq!(ensure_closed_code_fences(input), input);
    }

    #[test]
    fn test_ensure_closed_code_fences_zero_count_unchanged() {
        // 无 fence：不补
        let input = "普通段落文本";
        assert_eq!(ensure_closed_code_fences(input), input);
    }

    #[test]
    fn test_ensure_closed_code_fences_odd_count_appends_closer() {
        // 未闭合：fence 数为奇数，补一个闭合
        let input = "```rust\nlet x = 1;";
        let result = ensure_closed_code_fences(input);
        assert_eq!(result, "```rust\nlet x = 1;\n```");
        // 验证补完后变成偶数（递归调用应不再追加）
        assert_eq!(ensure_closed_code_fences(&result), result);
    }

    #[test]
    fn test_ensure_closed_code_fences_multiple_blocks() {
        // 多个代码块 + 末尾未闭合
        let input = "```rust\na\n```\n\ntext\n\n```python\nb";
        let result = ensure_closed_code_fences(input);
        assert!(result.ends_with("```"), "应在末尾补闭合 fence");
        assert_eq!(
            result.matches("```").count() % 2,
            0,
            "补完后 fence 数应为偶数"
        );
    }

    // ── Phase 2：parse_markdown_cached 续跑测试 ──────────────────────

    /// 测试辅助：把 segments 拼成纯文本便于断言。
    fn segments_to_text(segments: &[MarkdownSegment]) -> String {
        segments
            .iter()
            .flat_map(|s| match s {
                MarkdownSegment::Text(lines) => lines.clone(),
                MarkdownSegment::Table(_) => vec![],
            })
            .flat_map(|l| l.spans)
            .map(|s| s.content.into_owned())
            .collect()
    }

    #[test]
    fn test_cached_parse_matches_full_parse_on_first_call() {
        // 首次调用（cache 空）：输出应与全量 parse_markdown 一致
        let mut cache = MarkdownRenderCache::default();
        let input = "para1\n\npara2";
        let cached = parse_markdown_cached(input, 80, Palette::default(), &mut cache);
        let full = parse_markdown(input, 80, Palette::default());
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "首次调用 cached 与 full 输出应一致"
        );
    }

    #[test]
    fn test_cached_parse_reuses_prefix_on_append() {
        // 流式追加：text1 → text1+text2，cache 应命中 stable_text 前缀
        let mut cache = MarkdownRenderCache::default();

        // 第一次：一个完整 paragraph（以 \n\n 结尾，触发持久化）
        let t1 = "para1\n\n";
        let _r1 = parse_markdown_cached(t1, 80, Palette::default(), &mut cache);
        assert!(
            cache.stable_text_len() > 0,
            "首次以 \\n\\n 结尾应持久化 stable_text"
        );
        assert_eq!(
            cache.stable_processed_block_count(),
            1,
            "应处理 1 个 block（Paragraph）"
        );

        // 第二次：追加 para2，仍以 t1 为前缀
        let t2 = "para1\n\npara2";
        let r2 = parse_markdown_cached(t2, 80, Palette::default(), &mut cache);
        let full2 = parse_markdown(t2, 80, Palette::default());
        assert_eq!(
            segments_to_text(&r2),
            segments_to_text(&full2),
            "续跑输出应与全量一致"
        );
        // stable_text 不应扩展（t2 不以 \n 结尾）
        assert_eq!(
            cache.stable_text_len(),
            t1.len(),
            "t2 不以 \\n 结尾，stable_text 不应扩展"
        );
    }

    #[test]
    fn test_cached_parse_invalidates_on_width_change() {
        // width 变化 → cache 失效
        let mut cache = MarkdownRenderCache::default();
        let t1 = "para1\n\n";
        let _r1 = parse_markdown_cached(t1, 80, Palette::default(), &mut cache);
        assert!(cache.stable_text_len() > 0);

        // width 从 80 改到 60：cache 应失效（can_reuse = false）
        let t2 = "para1\n\npara2";
        let r2 = parse_markdown_cached(t2, 60, Palette::default(), &mut cache);
        let full2 = parse_markdown(t2, 60, Palette::default());
        assert_eq!(
            segments_to_text(&r2),
            segments_to_text(&full2),
            "width 变化后应全量重跑，输出一致"
        );
    }

    #[test]
    fn test_cached_parse_invalidates_on_palette_change() {
        // palette 变化 → cache 失效
        let mut cache = MarkdownRenderCache::default();
        let t1 = "para1\n\n";
        let p1 = Palette::default();
        let _r1 = parse_markdown_cached(t1, 80, p1, &mut cache);
        assert!(cache.stable_text_len() > 0);

        // 修改 palette（替换 fg 颜色）
        let mut p2 = Palette::default();
        p2.fg = ratatui::style::Color::Red;
        let t2 = "para1\n\npara2";
        let r2 = parse_markdown_cached(t2, 80, p2, &mut cache);
        let full2 = parse_markdown(t2, 80, p2);
        assert_eq!(
            segments_to_text(&r2),
            segments_to_text(&full2),
            "palette 变化后应全量重跑，输出一致"
        );
    }

    #[test]
    fn test_cached_parse_preserves_spacing_on_append() {
        // [回归测试] spacing 跨 block 边界正确——续跑时新 block 的 spacing 决策
        // 应与全量一致。多 paragraph + list 场景。
        let mut cache = MarkdownRenderCache::default();

        // 第一次：paragraph + list（以 \n\n 结尾）
        let t1 = "intro paragraph\n\n- item 1\n- item 2\n\n";
        let r1 = parse_markdown_cached(t1, 80, Palette::default(), &mut cache);
        let full1 = parse_markdown(t1, 80, Palette::default());
        assert_eq!(
            segments_to_text(&r1),
            segments_to_text(&full1),
            "首次输出应与全量一致"
        );

        // 第二次：追加新 paragraph
        let t2 = "intro paragraph\n\n- item 1\n- item 2\n\nnew paragraph";
        let r2 = parse_markdown_cached(t2, 80, Palette::default(), &mut cache);
        let full2 = parse_markdown(t2, 80, Palette::default());
        assert_eq!(
            segments_to_text(&r2),
            segments_to_text(&full2),
            "续跑追加 paragraph 后 spacing 应与全量一致"
        );
    }

    #[test]
    fn test_cached_parse_preserves_table_boundary_on_append() {
        // [回归测试] Table 触发 flush 的 spacing 跨续跑正确
        let mut cache = MarkdownRenderCache::default();

        // 第一次：paragraph + table（以 \n\n 结尾）
        let t1 = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n";
        let _r1 = parse_markdown_cached(t1, 80, Palette::default(), &mut cache);

        // 第二次：table 后追加 paragraph
        let t2 = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter table para";
        let r2 = parse_markdown_cached(t2, 80, Palette::default(), &mut cache);
        let full2 = parse_markdown(t2, 80, Palette::default());
        assert_eq!(
            segments_to_text(&r2),
            segments_to_text(&full2),
            "Table 后追加 paragraph 续跑输出应一致"
        );
        // 至少 2 个 segment（Text + Table + Text），但 segments_to_text 只统计 Text
        assert!(r2.len() >= 2, "应至少 2 个 segment（含 Table）");
    }

    #[test]
    fn test_cached_parse_multiple_progressive_appends() {
        // [回归测试] 多次渐进追加，cache 累积稳定，每次输出与全量一致
        let mut cache = MarkdownRenderCache::default();
        let steps = [
            "first\n\n",
            "first\n\nsecond\n\n",
            "first\n\nsecond\n\nthird\n\n",
            "first\n\nsecond\n\nthird\n\nfourth",
        ];
        let mut prev_stable_len = 0usize;
        for (i, text) in steps.iter().enumerate() {
            let cached = parse_markdown_cached(text, 80, Palette::default(), &mut cache);
            let full = parse_markdown(text, 80, Palette::default());
            assert_eq!(
                segments_to_text(&cached),
                segments_to_text(&full),
                "step {i} 输出应与全量一致"
            );
            // stable_text 应单调增长（每次新闭合 \n\n 时扩展）
            if text.ends_with("\n\n") {
                assert!(
                    cache.stable_text_len() >= prev_stable_len,
                    "step {i} stable_text 应单调增长"
                );
                prev_stable_len = cache.stable_text_len();
            }
        }
    }

    #[test]
    fn test_cached_parse_unclosed_code_block_not_persisted_but_still_correct() {
        // [回归测试] text 以未闭合 code block 结尾（不以 \n\n 结尾），cache 不应持久化
        // 错误的 stable_text，但仍应输出正确结果
        let mut cache = MarkdownRenderCache::default();

        // 第一次：未闭合 code block（末尾不是 \n）
        let t1 = "```rust\nlet x = 1;";
        let r1 = parse_markdown_cached(t1, 80, Palette::default(), &mut cache);
        let full1 = parse_markdown(t1, 80, Palette::default());
        assert_eq!(
            segments_to_text(&r1),
            segments_to_text(&full1),
            "未闭合 code block 输出应正确"
        );
        // ensure_closed_code_fences 补了 \n``` → sanitized 以 ``` 结尾，不以 \n 结尾
        // → stable_text 不持久化
        // 注意：sanitized = "```rust\nlet x = 1;\n```"，不以 \n 结尾
        // 所以 stable_text 应为空
        assert_eq!(
            cache.stable_text_len(),
            0,
            "未闭合 code block 不应持久化 stable_text"
        );
    }

    #[test]
    fn test_cached_parse_empty_input_no_cache_pollution() {
        // 空输入不应污染 cache
        let mut cache = MarkdownRenderCache::default();
        let _r = parse_markdown_cached("", 80, Palette::default(), &mut cache);
        assert_eq!(cache.stable_text_len(), 0, "空输入不应持久化");
    }

    #[test]
    fn test_cached_parse_does_not_persist_unstable_suffix() {
        // [回归测试] 流式期间 text 末尾是不稳定（非 \n 结尾），cache 不应扩展 stable_text，
        // 但应保留上次 \n\n 闭合时的 stable_text。下次相同前缀追加仍能命中。
        let mut cache = MarkdownRenderCache::default();

        // Step 1：闭合 paragraph
        let t1 = "para1\n\n";
        let _r1 = parse_markdown_cached(t1, 80, Palette::default(), &mut cache);
        let stable_len_after_t1 = cache.stable_text_len();
        assert!(stable_len_after_t1 > 0);

        // Step 2：追加半个 paragraph（不以 \n 结尾）
        let t2 = "para1\n\npara2 half";
        let _r2 = parse_markdown_cached(t2, 80, Palette::default(), &mut cache);
        assert_eq!(
            cache.stable_text_len(),
            stable_len_after_t1,
            "追加非闭合内容不应扩展 stable_text"
        );

        // Step 3：继续追加（仍以 t1 为前缀）
        let t3 = "para1\n\npara2 half continued";
        let r3 = parse_markdown_cached(t3, 80, Palette::default(), &mut cache);
        let full3 = parse_markdown(t3, 80, Palette::default());
        assert_eq!(
            segments_to_text(&r3),
            segments_to_text(&full3),
            "多次追加后仍应正确"
        );
    }
}
