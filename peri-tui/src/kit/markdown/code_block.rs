use std::sync::LazyLock;

use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_kit_markdown::MarkdownTheme;
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};

// ── syntect 全局单例 ───────────────────────────────────────────────

pub(crate) static SYNTAX_SET: LazyLock<SyntaxSet> =
    LazyLock::new(SyntaxSet::load_defaults_newlines);
pub(crate) static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

// ── 代码块高亮 ──────────────────────────────────────────────────────

pub(crate) fn highlight_code_block(lang: &str, raw_lines: &[String]) -> Option<Vec<Line<'static>>> {
    let ss = &*SYNTAX_SET;
    let syntax = ss.find_syntax_by_token(lang)?;
    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    let mut result = Vec::with_capacity(raw_lines.len());
    for line_text in raw_lines {
        let ranges = highlighter.highlight_line(line_text, ss).ok()?;
        let spans: Vec<Span<'static>> = ranges
            .iter()
            .map(|(style, text)| {
                let color = ratatui::style::Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                );
                Span::styled(text.to_string(), Style::default().fg(color))
            })
            .collect();
        result.push(Line::from(spans));
    }
    Some(result)
}

pub(crate) fn code_block_lines(
    lang: &str,
    raw_lines: &[String],
    theme: &MarkdownTheme,
) -> Vec<Line<'static>> {
    let lang_clean = lang.trim();
    let highlighted = highlight_code_block(lang_clean, raw_lines);

    if raw_lines.len() == 1 {
        // 单行代码块：inline code style
        if let Some(hl_lines) = highlighted {
            return hl_lines;
        }
        return vec![Line::from(Span::styled(
            raw_lines[0].clone(),
            theme.inline_code_style,
        ))];
    }

    // 多行代码块：每行加 `│ ` 前缀
    let prefix_style = theme.rule_style;
    let prefix = Span::styled("│ ", prefix_style);

    if let Some(hl_lines) = highlighted {
        hl_lines
            .into_iter()
            .map(|line| {
                let mut spans = vec![prefix.clone()];
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect()
    } else {
        raw_lines
            .iter()
            .map(|raw| {
                Line::from(vec![
                    prefix.clone(),
                    Span::styled(raw.clone(), Style::default()),
                ])
            })
            .collect()
    }
}
