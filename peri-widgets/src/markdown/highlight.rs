use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use std::sync::LazyLock;
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};

pub static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
pub static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// 对多行代码块进行语法高亮，返回着色后的 Line 列表。
/// 当语言标签未识别时返回 None，调用方应回退到统一颜色渲染。
pub fn highlight_code_block(lang: &str, lines: &[String]) -> Option<Vec<Line<'static>>> {
    let ss = &*SYNTAX_SET;
    let syntax = ss.find_syntax_by_token(lang)?;
    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    let mut result = Vec::with_capacity(lines.len());
    for line_text in lines {
        let mut spans = Vec::new();

        let ranges = highlighter.highlight_line(line_text, ss).ok()?;
        for (style, text) in ranges {
            let color = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            spans.push(Span::styled(text.to_string(), Style::default().fg(color)));
        }
        result.push(Line::from(spans));
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("highlight_test.rs");
}
