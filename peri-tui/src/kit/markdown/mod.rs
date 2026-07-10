//! Markdown 解析（kit 路径专用）。
//!
//! 仅暴露 view_render 需要的最小接口（`parse_markdown` +
//! `parse_markdown_default`），底层委托给 `peri_widgets::markdown`。
//! Legacy `ui::markdown` 模块（含 `ContentBlockView` 增量渲染逻辑）
//! 保留在 ui/ 内，待 ui 整片删除时一并清理。

use peri_widgets::DefaultMarkdownTheme;
use ratatui::text::Text;

static THEME: DefaultMarkdownTheme = DefaultMarkdownTheme;

/// 解析 markdown 文本为 ratatui Text。
pub fn parse_markdown(input: &str, max_width: usize) -> Text<'static> {
    peri_widgets::markdown::parse_markdown(input, &THEME, max_width)
}

/// 解析 markdown 文本为 ratatui Text（默认宽度 80）。
pub fn parse_markdown_default(input: &str) -> Text<'static> {
    parse_markdown(input, 80)
}
