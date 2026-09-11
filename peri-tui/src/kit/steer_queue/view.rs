use std::sync::{Arc, Mutex};

use peri_theme::semantic::SemanticTokens;
use ratatui_kit::ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{SteerItemState, SteerQueueAction, SteerQueueItem};
use crate::kit::terminal_caps::SymbolSet;

pub(super) const HEADER_ROWS: u16 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Selection {
    All,
    Expand,
    Dispatch(String),
    TakeBack(String),
}

impl Selection {
    pub(super) fn item_id(&self) -> Option<&str> {
        match self {
            Self::Dispatch(id) | Self::TakeBack(id) => Some(id),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct ViewState {
    pub(super) expanded: bool,
    pub(super) offset: usize,
    pub(super) selection: Option<Selection>,
}

#[derive(Clone)]
pub(super) struct Control {
    pub(super) area: Rect,
    pub(super) selection: Selection,
    pub(super) enabled: bool,
}

#[derive(Clone, Default)]
pub(super) struct QueueFrame {
    pub(super) area: Rect,
    pub(super) controls: Vec<Control>,
    pub(super) rows: Vec<(String, Rect)>,
    pub(super) dispatch_ids: Vec<String>,
    pub(super) offset: usize,
}

impl QueueFrame {
    pub(super) fn contains(&self, x: u16, y: u16) -> bool {
        contains(self.area, x, y)
    }

    pub(super) fn hit(&self, x: u16, y: u16) -> Option<&Control> {
        self.controls
            .iter()
            .find(|control| contains(control.area, x, y))
    }

    pub(super) fn row_at(&self, y: u16) -> Option<&str> {
        self.rows
            .iter()
            .find(|(_, area)| area.y == y)
            .map(|(id, _)| id.as_str())
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

pub(super) fn selections(items: &[SteerQueueItem], max_rows: usize) -> Vec<Selection> {
    let mut targets = Vec::new();
    if items
        .iter()
        .any(|item| item.state == SteerItemState::Queued)
    {
        targets.push(Selection::All);
    }
    if items.len() > max_rows {
        targets.push(Selection::Expand);
    }
    for item in items
        .iter()
        .filter(|item| item.state == SteerItemState::Queued)
    {
        targets.push(Selection::Dispatch(item.id.clone()));
        targets.push(Selection::TakeBack(item.id.clone()));
    }
    targets
}

pub(super) fn validated_action(
    selection: &Selection,
    snapshot: &[String],
    items: &[SteerQueueItem],
) -> Option<SteerQueueAction> {
    let queued = |id: &str| {
        items
            .iter()
            .any(|item| item.id == id && item.state == SteerItemState::Queued)
    };
    match selection {
        Selection::All => {
            let ids: Vec<_> = snapshot.iter().filter(|id| queued(id)).cloned().collect();
            (!ids.is_empty()).then_some(SteerQueueAction::Dispatch { ids })
        }
        Selection::Dispatch(id) if queued(id) => Some(SteerQueueAction::Dispatch {
            ids: vec![id.clone()],
        }),
        Selection::TakeBack(id) if queued(id) => {
            Some(SteerQueueAction::TakeBack { id: id.clone() })
        }
        _ => None,
    }
}

pub(super) fn preview(text: &str, width: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.width() <= width {
        return normalized;
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut columns = 0;
    for grapheme in normalized.graphemes(true) {
        let next = columns + grapheme.width();
        if next > width - 1 {
            break;
        }
        result.push_str(grapheme);
        columns = next;
    }
    result.push('…');
    result
}

#[derive(Clone)]
pub(super) struct QueueView {
    pub(super) items: Vec<SteerQueueItem>,
    pub(super) max_rows: usize,
    pub(super) state: ViewState,
    pub(super) symbols: SymbolSet,
    pub(super) semantic: SemanticTokens,
    pub(super) separator_style: Style,
    pub(super) title: String,
    pub(super) frame: Arc<Mutex<QueueFrame>>,
}

impl QueueView {
    fn action_widths(&self) -> (u16, u16) {
        let width = |label: &str| {
            label
                .width()
                .max(self.symbols.running.width())
                .saturating_add(2)
                .min(usize::from(u16::MAX)) as u16
        };
        (width(self.symbols.send), width(self.symbols.take_back))
    }

    pub(super) fn layout(&self, area: Rect) -> QueueFrame {
        if self.items.is_empty() || area.is_empty() || area.height < HEADER_ROWS {
            return QueueFrame::default();
        }
        let visible =
            usize::from(area.height.saturating_sub(HEADER_ROWS)).min(if self.state.expanded {
                self.items.len()
            } else {
                self.max_rows
            });
        let mut offset = if self.state.expanded {
            self.state
                .offset
                .min(self.items.len().saturating_sub(visible))
        } else {
            0
        };
        if self.state.expanded
            && let Some(id) = self.state.selection.as_ref().and_then(Selection::item_id)
            && let Some(index) = self.items.iter().position(|item| item.id == id)
        {
            if index < offset {
                offset = index;
            }
            if index >= offset.saturating_add(visible) {
                offset = index.saturating_add(1).saturating_sub(visible);
            }
        }
        let dispatch_ids: Vec<_> = self
            .items
            .iter()
            .filter(|item| item.state == SteerItemState::Queued)
            .map(|item| item.id.clone())
            .collect();
        let mut frame = QueueFrame {
            area,
            dispatch_ids,
            offset,
            ..QueueFrame::default()
        };
        let header_y = area.y.saturating_add(1);
        let all_width =
            (self.symbols.send_all.width().saturating_add(2)).min(usize::from(area.width)) as u16;
        frame.controls.push(Control {
            area: Rect::new(
                area.right().saturating_sub(all_width),
                header_y,
                all_width,
                1,
            ),
            selection: Selection::All,
            enabled: !frame.dispatch_ids.is_empty(),
        });
        if self.items.len() > self.max_rows {
            frame.controls.push(Control {
                area: Rect::new(
                    area.x,
                    header_y,
                    3.min(area.width.saturating_sub(all_width)),
                    1,
                ),
                selection: Selection::Expand,
                enabled: true,
            });
        }
        let (send_width, edit_width) = self.action_widths();
        let actions_width = send_width.saturating_add(edit_width).saturating_add(1);
        for (row, item) in self.items.iter().skip(offset).take(visible).enumerate() {
            let y = area
                .y
                .saturating_add(HEADER_ROWS)
                .saturating_add(row as u16);
            let row_area = Rect::new(area.x, y, area.width, 1);
            frame.rows.push((item.id.clone(), row_area));
            if area.width >= actions_width {
                frame.controls.push(Control {
                    area: Rect::new(area.right().saturating_sub(actions_width), y, send_width, 1),
                    selection: Selection::Dispatch(item.id.clone()),
                    enabled: item.state == SteerItemState::Queued,
                });
                frame.controls.push(Control {
                    area: Rect::new(area.right().saturating_sub(edit_width), y, edit_width, 1),
                    selection: Selection::TakeBack(item.id.clone()),
                    enabled: item.state == SteerItemState::Queued,
                });
            }
        }
        frame
    }
}

impl Widget for QueueView {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let frame = self.layout(area);
        let normal = Style::default().fg(self.semantic.text.secondary);
        buffer.set_style(area, normal);
        if !frame.area.is_empty() {
            Block::default()
                .borders(Borders::TOP)
                .border_style(self.separator_style)
                .render(area, buffer);
            let title_start = if self.items.len() > self.max_rows {
                3.min(area.width)
            } else {
                0
            };
            let all_start = frame.controls[0].area.x;
            Paragraph::new(preview(
                &self.title,
                usize::from(all_start.saturating_sub(area.x).saturating_sub(title_start)),
            ))
            .style(Style::default().fg(self.semantic.text.muted))
            .render(
                Rect::new(
                    area.x.saturating_add(title_start),
                    area.y.saturating_add(1),
                    all_start.saturating_sub(area.x).saturating_sub(title_start),
                    1,
                ),
                buffer,
            );
            let (send_width, edit_width) = self.action_widths();
            let actions_width = send_width.saturating_add(edit_width).saturating_add(1);
            for (id, row) in &frame.rows {
                if let Some(item) = self.items.iter().find(|item| item.id == *id) {
                    let text_width = if row.width >= actions_width {
                        row.width.saturating_sub(actions_width.saturating_add(1))
                    } else {
                        row.width
                    };
                    Paragraph::new(preview(&item.text, usize::from(text_width)))
                        .render(Rect::new(row.x, row.y, text_width, 1), buffer);
                }
            }
            for control in &frame.controls {
                let item_state = control
                    .selection
                    .item_id()
                    .and_then(|id| self.items.iter().find(|item| item.id == id))
                    .map(|item| item.state);
                let label = match &control.selection {
                    Selection::All => format!(" {} ", self.symbols.send_all),
                    Selection::Expand => format!(
                        " {} ",
                        if self.state.expanded {
                            self.symbols.expanded
                        } else {
                            self.symbols.collapsed
                        }
                    ),
                    Selection::Dispatch(_) => format!(
                        " {} ",
                        if matches!(
                            item_state,
                            Some(SteerItemState::Submitting | SteerItemState::Dispatching)
                        ) {
                            self.symbols.running
                        } else {
                            self.symbols.send
                        }
                    ),
                    Selection::TakeBack(_) => format!(
                        " {} ",
                        if item_state == Some(SteerItemState::Withdrawing) {
                            self.symbols.running
                        } else {
                            self.symbols.take_back
                        }
                    ),
                };
                let mut style = Style::default().fg(if control.enabled {
                    self.semantic.text.muted
                } else {
                    self.semantic.text.dim
                });
                if self.state.selection.as_ref() == Some(&control.selection) && control.enabled {
                    style = style
                        .fg(self.semantic.text.primary)
                        .add_modifier(Modifier::UNDERLINED);
                }
                Paragraph::new(label)
                    .style(style)
                    .render(control.area, buffer);
            }
        }
        // 绘制与命中共用同一布局；事件读取已绘制快照，不用新列表索引推算旧帧。
        *self.frame.lock().expect("SteerQueue frame poisoned") = frame;
    }
}
