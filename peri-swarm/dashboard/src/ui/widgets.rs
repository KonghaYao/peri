//! Shared rendering helpers built on `peri-tui-components` and `ratatui`.
//!
//! Buttons, colors, the loading spinner and scroll behavior come from the
//! shared components. The single-line field state lives in the shared input
//! module; its bordered block is drawn here because the dashboard owns the
//! terminal cursor.
//!
//! Every helper draws into a [`Buffer`] rather than a [`ratatui::Frame`] so a
//! page can be rendered into an off-screen buffer, measured, and then blitted
//! through the scroll offset of the most recent frame.

use std::sync::OnceLock;

use peri_theme::theme::ThemeDefinition;
use peri_tui_components::button::{Button, ButtonChrome, ButtonState, ButtonTone};
use peri_tui_components::spinner::Spinner;
use peri_tui_components::text::{truncate_to_width, wrap_by_width};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Row, Table, Tabs, Widget, Wrap};

use crate::text_field::TextField;

/// Label column width for label/value rows.
pub const LABEL_WIDTH: u16 = 20;

/// Dashboard theme. A single built-in dark theme keeps the shell deterministic.
pub fn theme() -> &'static ThemeDefinition {
    static THEME: OnceLock<ThemeDefinition> = OnceLock::new();
    THEME.get_or_init(peri_theme::builtin::dark_theme)
}

/// Page and section titles.
#[must_use]
pub fn title_style() -> Style {
    Style::default()
        .fg(theme().semantic.text.primary)
        .add_modifier(Modifier::BOLD)
}

/// Section headings.
#[must_use]
pub fn section_style() -> Style {
    Style::default()
        .fg(theme().semantic.accent)
        .add_modifier(Modifier::BOLD)
}

/// Secondary body text.
#[must_use]
pub fn muted_style() -> Style {
    Style::default().fg(theme().semantic.text.muted)
}

/// Low-emphasis hints.
#[must_use]
pub fn dim_style() -> Style {
    Style::default().fg(theme().semantic.text.dim)
}

/// Error text.
#[must_use]
pub fn error_style() -> Style {
    Style::default().fg(theme().semantic.status.error)
}

/// Warning text.
#[must_use]
pub fn warning_style() -> Style {
    Style::default().fg(theme().semantic.status.warning)
}

/// Success text.
#[must_use]
pub fn success_style() -> Style {
    Style::default().fg(theme().semantic.status.success)
}

/// Selected sidebar row.
#[must_use]
pub fn selected_style() -> Style {
    Style::default()
        .fg(theme().semantic.accent)
        .bg(theme().semantic.surface.selection)
        .add_modifier(Modifier::BOLD)
}

/// Border color for a focused or idle field/panel.
#[must_use]
pub fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(theme().semantic.border.active)
    } else {
        Style::default().fg(theme().semantic.border.default)
    }
}

/// Semantic status colors for chips and dots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusTone {
    /// Healthy/success.
    Ok,
    /// Degraded/warning.
    Warning,
    /// Failure.
    Error,
    /// Unknown/muted.
    Muted,
}

/// Style for a status tone.
#[must_use]
pub fn tone_style(tone: StatusTone) -> Style {
    match tone {
        StatusTone::Ok => success_style(),
        StatusTone::Warning => warning_style(),
        StatusTone::Error => error_style(),
        StatusTone::Muted => muted_style(),
    }
}

/// Decimal health/status dot.
#[must_use]
pub fn status_dot(tone: StatusTone) -> Span<'static> {
    Span::styled("●", tone_style(tone))
}

/// A dot plus a label.
#[must_use]
pub fn status_chip(label: &str, tone: StatusTone) -> Line<'static> {
    Line::from(vec![
        status_dot(tone),
        Span::raw(" "),
        Span::styled(label.to_owned(), tone_style(tone)),
    ])
}

/// A vertical cursor over a rectangular content area.
///
/// Rows are handed out top to bottom. The rectangle handed back is clipped to
/// the area, but the cursor advances by the requested height so a caller can
/// measure content that extends past a small terminal.
///
/// A windowed cursor ([`Rows::window`]) keeps the full content height for
/// scrolling while drawing only the visible slice: content row `scroll` lands
/// on buffer row `margin`, and rows outside `scroll..scroll + height` are
/// handed back empty. The leading margin lets a block that starts just above
/// the viewport still be drawn from its first line and be clipped by the blit.
#[derive(Clone, Copy, Debug)]
pub struct Rows {
    area: Rect,
    y: u16,
    window: Option<Window>,
}

/// Visible slice of a scrolling page.
#[derive(Clone, Copy, Debug)]
struct Window {
    /// First content row shown below the leading margin.
    scroll: u16,
    /// Scratch rows reserved above the viewport.
    margin: u16,
    /// Viewport height in rows.
    height: u16,
}

impl Rows {
    /// Start at the top of `area`.
    #[must_use]
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            y: area.y,
            window: None,
        }
    }

    /// Start a scrolling page: content row `scroll` is drawn at buffer row
    /// `margin`, with `height` viewport rows below it.
    #[must_use]
    pub fn window(area: Rect, scroll: u16, margin: u16, height: u16) -> Self {
        Self {
            area,
            y: area.y,
            window: Some(Window {
                scroll,
                margin,
                height,
            }),
        }
    }

    /// Total height used so far, relative to the area top.
    #[must_use]
    pub fn used(&self) -> u16 {
        self.y.saturating_sub(self.area.y)
    }

    /// Remaining width.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.area.width
    }

    /// Allocate `height` rows.
    pub fn take(&mut self, height: u16) -> Rect {
        let top = self.y;
        self.y = self.y.saturating_add(height);
        let Some(window) = self.window else {
            let available = self.area.bottom().saturating_sub(top);
            let visible = height.min(available);
            return Rect::new(self.area.x, top, self.area.width, visible);
        };
        let bottom = top.saturating_add(height);
        let view_end = window.scroll.saturating_add(window.height);
        if height == 0 || bottom <= window.scroll || top >= view_end {
            return Rect::new(self.area.x, 0, self.area.width, 0);
        }
        let draw_y = i32::from(top) - i32::from(window.scroll) + i32::from(window.margin);
        if draw_y < 0 {
            // Only a block taller than the leading margin can start this far
            // above the viewport; fall back to its visible tail.
            let hidden = u16::try_from(-draw_y).unwrap_or(u16::MAX);
            if hidden >= height {
                return Rect::new(self.area.x, 0, self.area.width, 0);
            }
            return Rect::new(self.area.x, 0, self.area.width, height - hidden);
        }
        let draw_y = u16::try_from(draw_y).unwrap_or(u16::MAX);
        Rect::new(self.area.x, draw_y, self.area.width, height)
    }

    /// Allocate one row.
    pub fn line(&mut self) -> Rect {
        self.take(1)
    }

    /// Advance without drawing.
    pub fn skip(&mut self, height: u16) {
        self.y = self.y.saturating_add(height);
    }
}

/// Chrome used for each button tone.
///
/// Primary actions read as a solid block; secondary and destructive actions
/// stay plain text so a page has one clear visual accent.
#[must_use]
pub fn button_chrome(tone: ButtonTone) -> ButtonChrome {
    match tone {
        ButtonTone::Primary => ButtonChrome::Filled,
        ButtonTone::Secondary | ButtonTone::Danger => ButtonChrome::Plain,
    }
}

/// Width of a button label for `tone`, including the filled chrome padding.
#[must_use]
pub fn button_width(label: &str, tone: ButtonTone) -> u16 {
    Button::new(label, button_chrome(tone), tone, ButtonState::Normal).width()
}

/// A button rectangle clamped to the row it is placed on.
#[must_use]
pub fn button_rect(row: Rect, x: u16, label: &str, tone: ButtonTone) -> Rect {
    let width = button_width(label, tone).min(row.right().saturating_sub(x));
    let height = if row.height == 0 { 0 } else { 1 };
    Rect::new(x, row.y, width, height)
}

/// Render a button using the shared component for text and style.
pub fn render_button(buf: &mut Buffer, rect: Rect, label: &str, focused: bool, tone: ButtonTone) {
    let state = if focused {
        ButtonState::Focused
    } else {
        ButtonState::Normal
    };
    render_button_state(buf, rect, label, state, tone);
}

/// Render a button in an explicit state.
///
/// Drawing only: the caller still owns the hit region.
pub fn render_button_state(
    buf: &mut Buffer,
    rect: Rect,
    label: &str,
    state: ButtonState,
    tone: ButtonTone,
) {
    Button::new(label, button_chrome(tone), tone, state).render(buf, rect, theme());
}

/// Render a single-line text field with a block, optional masking, and a caret.
///
/// Returns the caret position in buffer coordinates when the field is focused.
pub fn render_field(
    buf: &mut Buffer,
    rect: Rect,
    field: &TextField,
    focused: bool,
    masked: bool,
    placeholder: &str,
) -> Option<Position> {
    if rect.is_empty() {
        return None;
    }
    let block = Block::bordered().border_style(border_style(focused));
    let inner = block.inner(rect);
    block.render(rect, buf);
    if inner.is_empty() {
        return None;
    }
    let display = field.display(inner.width, masked);
    if display.text.is_empty() {
        if !placeholder.is_empty() {
            Paragraph::new(truncate_to_width(placeholder, usize::from(inner.width)))
                .style(dim_style())
                .render(inner, buf);
        }
    } else if let Some((start, end)) = display.selection {
        Paragraph::new(styled_selection(&display.text, start, end)).render(inner, buf);
    } else {
        Paragraph::new(display.text)
            .style(title_style())
            .render(inner, buf);
    }
    if focused {
        Some(Position::new(
            inner.x.saturating_add(display.caret_col),
            inner.y,
        ))
    } else {
        None
    }
}

/// Split a visible field window into styled runs for an active text selection.
///
/// The range is expressed in terminal cells relative to the first visible cell,
/// matching [`crate::text_field::FieldDisplay::selection`].
fn styled_selection(text: &str, start: u16, end: u16) -> Line<'static> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_selected = false;
    let mut cell = 0u16;
    for character in text.chars() {
        let width = u16::try_from(Span::raw(character.to_string()).width()).unwrap_or(u16::MAX);
        let selected = cell >= start && cell < end;
        if !run.is_empty() && selected != run_selected {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_selected {
                    selected_style()
                } else {
                    title_style()
                },
            ));
        }
        run_selected = selected;
        run.push(character);
        cell = cell.saturating_add(width);
    }
    if !run.is_empty() {
        spans.push(Span::styled(
            run,
            if run_selected {
                selected_style()
            } else {
                title_style()
            },
        ));
    }
    Line::from(spans)
}

/// Label/value row used by Settings and placeholder pages.
pub fn render_label_value(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    value: &str,
    value_style: Style,
) {
    if area.is_empty() {
        return;
    }
    let columns = Layout::horizontal([
        Constraint::Length(LABEL_WIDTH.min(area.width)),
        Constraint::Min(0),
    ])
    .split(area);
    Paragraph::new(label)
        .style(muted_style())
        .render(columns[0], buf);
    if !columns[1].is_empty() {
        Paragraph::new(truncate_to_width(value, usize::from(columns[1].width)))
            .style(value_style)
            .render(columns[1], buf);
    }
}

/// Section heading.
pub fn render_section(buf: &mut Buffer, area: Rect, label: &str) {
    if area.is_empty() {
        return;
    }
    Paragraph::new(label)
        .style(section_style())
        .render(area, buf);
}

/// Wrapped paragraph.
pub fn render_body(buf: &mut Buffer, area: Rect, body: &str, style: Style) {
    if area.is_empty() {
        return;
    }
    Paragraph::new(body.to_owned())
        .style(style)
        .wrap(Wrap { trim: true })
        .render(area, buf);
}

/// Wrapped line count for `text` at `width`, matching [`render_body`].
#[must_use]
pub fn wrapped_height(text: &str, width: u16) -> u16 {
    let lines = wrap_by_width(text, usize::from(width.max(1)));
    u16::try_from(lines.len()).unwrap_or(u16::MAX)
}

/// Word-wrap `text` into display lines that fit `width` columns.
///
/// Unlike [`wrap_by_width`] this prefers spaces, so prose does not break in the
/// middle of a word. A word wider than the column is still split, and an empty
/// string yields one empty line. Callers that need to scroll one line at a time
/// (the help overlay) use this instead of [`render_body`]'s internal wrapping.
#[must_use]
pub fn wrap_words(text: &str, width: u16) -> Vec<String> {
    let max = usize::from(width);
    if max == 0 {
        return vec![text.to_owned()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut rest = word;
        while display_width(rest) > max {
            let head = peri_tui_components::text::prefix_by_width(rest, max);
            if head.is_empty() {
                // A single grapheme wider than the column occupies its own row.
                let character = rest.chars().next().unwrap_or_default();
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                lines.push(character.to_string());
                rest = &rest[character.len_utf8()..];
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(head.clone());
            rest = &rest[head.len()..];
        }
        if rest.is_empty() {
            continue;
        }
        if current.is_empty() {
            current.push_str(rest);
        } else if display_width(&format!("{current} {rest}")) <= max {
            current.push(' ');
            current.push_str(rest);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(rest);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn display_width(value: &str) -> usize {
    Span::raw(value).width()
}

/// Braille frames for loading indicators.
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Render a loading line: spinner glyph, one space, then the unchanged label.
pub fn render_loading(buf: &mut Buffer, area: Rect, frame: u64, label: &str, style: Style) {
    if area.is_empty() {
        return;
    }
    let line = Spinner::new(SPINNER_FRAMES, frame)
        .icon_style(style)
        .text(Line::from(Span::styled(label.to_owned(), style)))
        .line();
    Paragraph::new(line).render(area, buf);
}

/// Centered line.
pub fn render_centered(buf: &mut Buffer, area: Rect, line: Line<'static>) {
    if area.is_empty() {
        return;
    }
    Paragraph::new(line)
        .alignment(Alignment::Center)
        .render(area, buf);
}

/// Non-interactive tab bar (session detail uses it in a later stage).
pub fn render_tabs(buf: &mut Buffer, area: Rect, titles: &[&str], selected: usize) {
    if area.is_empty() || titles.is_empty() {
        return;
    }
    let tabs = Tabs::new(titles.iter().map(|title| Line::from(*title)))
        .select(selected.min(titles.len() - 1))
        .divider(Span::styled(" | ", dim_style()))
        .style(muted_style())
        .highlight_style(selected_style());
    tabs.render(area, buf);
}

/// Minimal table renderer for later stages; column widths are caller-owned.
pub fn render_table(
    buf: &mut Buffer,
    area: Rect,
    headers: &[&str],
    rows: &[Vec<String>],
    widths: &[Constraint],
) {
    if area.is_empty() {
        return;
    }
    let header = Row::new(headers.iter().map(|header| (*header).to_owned())).style(section_style());
    let body = rows
        .iter()
        .map(|row| Row::new(row.to_vec()).style(muted_style()));
    let table = Table::new(body, widths.to_vec())
        .header(header)
        .column_spacing(2);
    Widget::render(table, area, buf);
}

/// Table body without a header, for chunks that continue a table above.
pub fn render_table_body(
    buf: &mut Buffer,
    area: Rect,
    rows: &[Vec<String>],
    widths: &[Constraint],
) {
    if area.is_empty() {
        return;
    }
    let body = rows
        .iter()
        .map(|row| Row::new(row.clone()).style(muted_style()));
    let table = Table::new(body, widths.to_vec()).column_spacing(2);
    Widget::render(table, area, buf);
}

/// Truncate text to `width` cells.
#[must_use]
pub fn fit(text: &str, width: u16) -> String {
    truncate_to_width(text, usize::from(width))
}

/// Draw a horizontal rule as a dim line of `─` characters.
pub fn render_rule(buf: &mut Buffer, area: Rect) {
    if area.is_empty() {
        return;
    }
    buf.set_string(
        area.x,
        area.y,
        "─".repeat(usize::from(area.width)),
        dim_style(),
    );
}

#[cfg(test)]
#[path = "widgets_test.rs"]
mod tests;
