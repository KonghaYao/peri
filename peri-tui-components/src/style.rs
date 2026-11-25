//! Pure style degradation for `NO_COLOR` output.
//!
//! Only color fields are removed: modifiers, symbols, text and alignment survive so
//! state never depends on color alone. The host decides when to apply this pass.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Strip fg/bg/underline colors from a line while keeping modifiers and alignment.
#[must_use]
pub fn strip_line_colors(line: &Line<'static>) -> Line<'static> {
    // Line-level and span-level styles follow the same rule: colors out, modifiers in.
    let line_style = strip_style_color(line.style);
    let spans = line
        .spans
        .iter()
        .map(|span| Span::styled(span.content.clone(), strip_style_color(span.style)))
        .collect();
    Line {
        spans,
        alignment: line.alignment,
        style: line_style,
    }
}

/// Strip the color fields of one style, keeping `add_modifier`/`sub_modifier`.
#[must_use]
pub fn strip_style_color(style: Style) -> Style {
    Style {
        fg: None,
        bg: None,
        underline_color: None,
        add_modifier: style.add_modifier,
        sub_modifier: style.sub_modifier,
    }
}

/// Strip fg/bg/underline colors from every cell of `buffer`, keeping modifiers.
///
/// Apply this once per frame after all components have drawn (the root hook), so
/// no surface needs its own color-stripping pass. Cell symbols, modifiers
/// (`BOLD`, `REVERSED`, ...) and diff options survive.
pub fn strip_buffer_colors(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
        cell.underline_color = Color::Reset;
    }
}

#[cfg(test)]
#[path = "style_test.rs"]
mod tests;
