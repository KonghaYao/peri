//! Pure style degradation for `NO_COLOR` output.
//!
//! Only color fields are removed: modifiers, symbols, text and alignment survive so
//! state never depends on color alone. The host decides when to apply this pass.

use ratatui::style::Style;
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

#[cfg(test)]
#[path = "style_test.rs"]
mod tests;
