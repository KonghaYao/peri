//! Pagination geometry and drawing.
//!
//! The host owns the data, the current page and hit testing. This module turns a
//! [`PaginationState`] into one row of labelled controls and paints them with
//! theme tokens. Every control rectangle covers exactly the cells of the label
//! it draws, so a host can register one hit region per enabled control without
//! recomputing geometry.
//!
//! The wide form shows page numbers with an ellipsis over each skipped range.
//! Every page number is padded to the decimal width of the highest page and the
//! whole window is centred inside a band reserved for the widest possible
//! window, so `Previous`, the band and `Next` keep their columns while paging.
//! When the reserved row is too narrow, it falls back to
//! `previous, current/total, next` so `Next` stays reachable; a row that is too
//! narrow even for the fallback is clipped.

use std::collections::BTreeSet;

use peri_theme::theme::ThemeDefinition;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::text::truncate_to_width;

/// Largest number of page-number controls the wide form draws.
///
/// The window contributes `2 * WINDOW_RADIUS + 1` numbers and the first and last
/// page add at most two more, so the two constants must stay related.
const MAX_PAGE_NUMBERS: usize = 7;
/// Page numbers drawn on each side of the current page.
const WINDOW_RADIUS: usize = 2;
/// Columns between two adjacent controls.
const GAP: u16 = 1;
/// Label of a skipped page range.
const ELLIPSIS: &str = "…";
/// Columns taken by one ellipsis control.
const ELLIPSIS_WIDTH: u16 = 1;

/// Where the pager is and what it can reach.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaginationState {
    /// Current page, 0-based. Clamped to `page_count - 1` when laid out.
    pub page: usize,
    /// Number of pages. `0` is treated as `1`.
    pub page_count: usize,
    /// Whether a page beyond `page_count` may exist (another fetch can extend
    /// the loaded set).
    pub has_more: bool,
}

impl PaginationState {
    /// Current page after clamping.
    #[must_use]
    pub fn current(&self) -> usize {
        self.page.min(page_total(self.page_count) - 1)
    }
}

/// Page count with an empty set counting as one page.
fn page_total(page_count: usize) -> usize {
    page_count.max(1)
}

/// Control labels supplied by the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaginationLabels<'a> {
    /// Label drawn for the previous-page control.
    pub previous: &'a str,
    /// Label drawn for the next-page control.
    pub next: &'a str,
}

/// What activating a control means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageAction {
    /// Show the previous page.
    Previous,
    /// Show the next page.
    Next,
    /// Show this page, 0-based.
    Page(usize),
    /// A skipped page range: drawn, never clickable.
    Gap,
}

/// One drawn control and the cells it occupies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaginationControl {
    /// Meaning of the control.
    pub action: PageAction,
    /// Cells covered by the drawn label, padding included.
    pub rect: Rect,
    /// Text drawn inside [`Self::rect`].
    pub label: String,
    /// Whether activating the control does something.
    pub enabled: bool,
    /// Whether this is the current page, drawn as a filled block.
    pub current: bool,
}

/// One row of pagination controls.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PaginationLayout {
    /// Controls in drawn order.
    pub controls: Vec<PaginationControl>,
    /// Total columns occupied by the row, from the left edge of the first
    /// control to the right edge of the last one. The wide form includes the
    /// blank columns reserved for the page-number band.
    pub width: u16,
}

/// One control before its cells are known.
struct Spec {
    action: PageAction,
    label: String,
    enabled: bool,
    current: bool,
}

/// Layout for `area`, one row high.
///
/// The wide form needs its full reserved width (including the page-number band
/// blanks) to fit; otherwise the compact `previous, current/total, next` form is
/// used, and when even that does not fit the labels are clipped at the right
/// edge. An empty area produces an empty layout.
#[must_use]
pub fn pagination_layout(
    area: Rect,
    state: PaginationState,
    labels: PaginationLabels<'_>,
) -> PaginationLayout {
    if area.is_empty() {
        return PaginationLayout::default();
    }
    let page_count = page_total(state.page_count);
    let current = state.current();
    let digits = page_digits(page_count);
    let reserved = reserved_band_width(page_count);
    let wide_width = label_width(labels.previous)
        .saturating_add(GAP)
        .saturating_add(reserved)
        .saturating_add(GAP)
        .saturating_add(label_width(labels.next));
    if wide_width <= area.width {
        return wide_layout(
            area,
            current,
            page_count,
            state.has_more,
            labels,
            digits,
            reserved,
        );
    }
    layout_specs(
        area,
        compact_specs(current, page_count, state.has_more, labels, digits),
    )
}

/// Place the wide form: fixed `previous`, a reserved band the window is centred
/// in, and fixed `next`.
fn wide_layout(
    area: Rect,
    current: usize,
    page_count: usize,
    has_more: bool,
    labels: PaginationLabels<'_>,
    digits: u16,
    reserved: u16,
) -> PaginationLayout {
    let previous_width = label_width(labels.previous);
    let band_left = area.x.saturating_add(previous_width).saturating_add(GAP);
    let band = band_specs(page_count, current, digits);
    let offset = reserved.saturating_sub(specs_width(&band)) / 2;

    let mut controls = Vec::with_capacity(band.len() + 2);
    controls.push(PaginationControl {
        action: PageAction::Previous,
        rect: Rect::new(area.x, area.y, previous_width, 1),
        label: labels.previous.to_owned(),
        enabled: current > 0,
        current: false,
    });
    let mut x = band_left.saturating_add(offset);
    for spec in band {
        let width = label_width(&spec.label);
        controls.push(PaginationControl {
            action: spec.action,
            rect: Rect::new(x, area.y, width, 1),
            label: spec.label,
            enabled: spec.enabled,
            current: spec.current,
        });
        x = x.saturating_add(width).saturating_add(GAP);
    }
    let next_x = band_left.saturating_add(reserved).saturating_add(GAP);
    let next_width = label_width(labels.next);
    controls.push(PaginationControl {
        action: PageAction::Next,
        rect: Rect::new(next_x, area.y, next_width, 1),
        label: labels.next.to_owned(),
        enabled: next_enabled(current, page_count, has_more),
        current: false,
    });

    PaginationLayout {
        width: next_x.saturating_add(next_width).saturating_sub(area.x),
        controls,
    }
}

/// Place specs left to right, clipping the row at the area's right edge.
fn layout_specs(area: Rect, specs: Vec<Spec>) -> PaginationLayout {
    let mut controls = Vec::with_capacity(specs.len());
    let right = area.right();
    let mut x = area.x;
    for spec in specs {
        if x >= right {
            break;
        }
        let width = label_width(&spec.label).min(right.saturating_sub(x));
        if width == 0 {
            break;
        }
        controls.push(PaginationControl {
            action: spec.action,
            rect: Rect::new(x, area.y, width, 1),
            label: spec.label,
            enabled: spec.enabled,
            current: spec.current,
        });
        x = x.saturating_add(width).saturating_add(GAP);
    }
    let width = controls
        .last()
        .map_or(0, |control| control.rect.right().saturating_sub(area.x));
    PaginationLayout { controls, width }
}

/// Draw every label into its rectangle.
///
/// Drawing only: the row is never cleared, so cells outside the controls keep
/// whatever the caller painted before. The host owns hit testing and focus.
pub fn render_pagination(buf: &mut Buffer, layout: &PaginationLayout, theme: &ThemeDefinition) {
    for control in &layout.controls {
        let rect = control.rect.intersection(buf.area);
        if rect.is_empty() {
            continue;
        }
        let text = truncate_to_width(&control.label, usize::from(rect.width));
        buf.set_string(rect.x, rect.y, text, control_style(control, theme));
    }
}

fn control_style(control: &PaginationControl, theme: &ThemeDefinition) -> Style {
    let semantic = &theme.semantic;
    if control.current {
        return Style::default()
            .fg(semantic.accent)
            .bg(semantic.surface.selection)
            .add_modifier(Modifier::BOLD);
    }
    if !control.enabled {
        return Style::default().fg(semantic.text.dim);
    }
    match control.action {
        PageAction::Page(_) => Style::default().fg(semantic.text.secondary),
        PageAction::Previous | PageAction::Next => Style::default().fg(semantic.accent),
        PageAction::Gap => Style::default().fg(semantic.text.dim),
    }
}

/// Page-number controls with ellipses, in draw order.
fn band_specs(page_count: usize, current: usize, digits: u16) -> Vec<Spec> {
    page_slots(page_count, current)
        .into_iter()
        .map(|slot| match slot {
            Some(page) => Spec {
                action: PageAction::Page(page),
                label: page_label(page, digits),
                enabled: true,
                current: page == current,
            },
            None => Spec {
                action: PageAction::Gap,
                label: ELLIPSIS.to_owned(),
                enabled: false,
                current: false,
            },
        })
        .collect()
}

/// Compact controls for rows too narrow for page numbers.
fn compact_specs(
    current: usize,
    page_count: usize,
    has_more: bool,
    labels: PaginationLabels<'_>,
    digits: u16,
) -> Vec<Spec> {
    vec![
        Spec {
            action: PageAction::Previous,
            label: labels.previous.to_owned(),
            enabled: current > 0,
            current: false,
        },
        Spec {
            action: PageAction::Page(current),
            label: format!(
                " {:>width$}/{} ",
                current + 1,
                page_count,
                width = usize::from(digits)
            ),
            enabled: false,
            current: true,
        },
        Spec {
            action: PageAction::Next,
            label: labels.next.to_owned(),
            enabled: next_enabled(current, page_count, has_more),
            current: false,
        },
    ]
}

/// Next is reachable while a later loaded page or an unfetched page exists.
fn next_enabled(current: usize, page_count: usize, has_more: bool) -> bool {
    current + 1 < page_count || has_more
}

/// Label for one page number, centred in a fixed `digits`-wide control.
fn page_label(page: usize, digits: u16) -> String {
    format!("{:^width$}", page + 1, width = usize::from(digits))
}

/// Decimal width of the highest page.
fn page_digits(page_count: usize) -> u16 {
    let mut digits = 1u16;
    let mut highest = page_count.max(1);
    while highest >= 10 {
        highest /= 10;
        digits += 1;
    }
    digits
}

/// Columns reserved for the page-number band, gaps included.
///
/// A window draws at most [`MAX_PAGE_NUMBERS`] numbers and at most two
/// ellipses. Eight pages can show two ellipses instead of a seventh number, so
/// their width never exceeds seven numbers plus one ellipsis; nine or more
/// pages can show seven numbers plus two ellipses. Reserving that width for
/// every current page keeps the surrounding controls in the same columns while
/// the window slides.
fn reserved_band_width(page_count: usize) -> u16 {
    let page_count = page_total(page_count);
    let (numbers, gaps) = if page_count <= MAX_PAGE_NUMBERS {
        (page_count, 0usize)
    } else if page_count == MAX_PAGE_NUMBERS + 1 {
        (MAX_PAGE_NUMBERS, 1usize)
    } else {
        (MAX_PAGE_NUMBERS, 2usize)
    };
    let units = numbers.saturating_add(gaps);
    let numbers_width = u16::try_from(numbers)
        .unwrap_or(u16::MAX)
        .saturating_mul(page_digits(page_count));
    let gaps_width = u16::try_from(gaps)
        .unwrap_or(u16::MAX)
        .saturating_mul(ELLIPSIS_WIDTH);
    let spacing = u16::try_from(units.saturating_sub(1))
        .unwrap_or(u16::MAX)
        .saturating_mul(GAP);
    numbers_width
        .saturating_add(gaps_width)
        .saturating_add(spacing)
}

/// Page-number slots in draw order; `None` marks a skipped range.
///
/// All pages show when they fit in [`MAX_PAGE_NUMBERS`]. Otherwise the current
/// page keeps [`WINDOW_RADIUS`] numbers on each side, the first and last page
/// are always shown, and the set stays within [`MAX_PAGE_NUMBERS`] because the
/// window replaces the boundary whenever they overlap.
fn page_slots(page_count: usize, current: usize) -> Vec<Option<usize>> {
    if page_count <= MAX_PAGE_NUMBERS {
        return (0..page_count).map(Some).collect();
    }
    let mut shown = BTreeSet::new();
    shown.insert(0);
    shown.insert(page_count - 1);
    let start = current.saturating_sub(WINDOW_RADIUS);
    let end = current
        .saturating_add(WINDOW_RADIUS)
        .min(page_count.saturating_sub(1));
    shown.extend(start..=end);

    let mut slots = Vec::with_capacity(shown.len() + 2);
    let mut previous: Option<usize> = None;
    for page in shown {
        if previous.is_some_and(|previous| page > previous.saturating_add(1)) {
            slots.push(None);
        }
        slots.push(Some(page));
        previous = Some(page);
    }
    slots
}

fn specs_width(specs: &[Spec]) -> u16 {
    let mut width = 0u16;
    for (index, spec) in specs.iter().enumerate() {
        if index > 0 {
            width = width.saturating_add(GAP);
        }
        width = width.saturating_add(label_width(&spec.label));
    }
    width
}

fn label_width(label: &str) -> u16 {
    u16::try_from(Span::raw(label).width()).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[path = "pagination_test.rs"]
mod tests;
