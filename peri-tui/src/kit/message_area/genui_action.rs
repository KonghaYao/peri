use std::sync::{Arc, Weak};
use std::time::Duration;

use peri_theme::theme::ThemeDefinition;
use peri_tui_components::{button::hit_test, genui::render::pressed_button_style};
use ratatui_kit::crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{buffer::Buffer, layout::Rect, style::Modifier};
use unicode_width::UnicodeWidthStr;

use super::selection::{SlotIndex, SlotLines};
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::{
    AT_MENTION_ACTIVE, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, INPUT_RESTORE_TEXT, NOTIFICATION,
    Notification, RENDER_HEARTBEAT, SLASH_HINT_ACTIVE, ViewModelsSnapshot,
};
use crate::kit::genui::view_state;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use peri_tui_components::genui::{GenUiAction, GenUiButtonInfo};

#[derive(Clone)]
pub(super) struct PressedGenuiKey {
    slot_index: usize,
    vm_hash: u64,
    epoch: u64,
    line_start: usize,
    x_start: u16,
    // 区分同一 VM 的内容重建（例如 Set/Reset），不延长缓存行的生命周期。
    source: Weak<SlotLines>,
}

impl PressedGenuiKey {
    fn matches(&self, other: &Self) -> bool {
        self.slot_index == other.slot_index
            && self.vm_hash == other.vm_hash
            && self.epoch == other.epoch
            && self.line_start == other.line_start
            && self.x_start == other.x_start
            && self.source.ptr_eq(&other.source)
    }
}

pub(super) struct GenUiButtonHit {
    row: u16,
    x_start: u16,
    x_end: u16,
    key: PressedGenuiKey,
    button: GenUiButtonInfo,
}

pub(super) fn button_hits(
    caches: &[VmCacheSlot],
    snapshot: &ViewModelsSnapshot,
    area: Rect,
    height: u16,
    scroll_y: usize,
    index: &SlotIndex,
) -> Vec<GenUiButtonHit> {
    let mut hits = Vec::new();
    let width = area.width.saturating_sub(1);
    if width == 0 {
        return hits;
    }
    for slot_index in index.visible_slots(scroll_y, height as usize) {
        let Some(slot) = caches.get(slot_index) else {
            continue;
        };
        let Some(TuiRenderUnit::TuiAssistantBubble(bubble)) = snapshot.items.get(slot_index) else {
            continue;
        };
        let Some(lines) = slot.lines.as_ref() else {
            continue;
        };
        let slot_start = index.slot_visual_start(slot_index).unwrap_or(0);
        let mut vm_hash = None;
        for button in &slot.genui_buttons {
            for (visual, start, end) in button_rows(slot, button, width) {
                let Some(row) = slot_start
                    .saturating_add(visual)
                    .checked_sub(scroll_y)
                    .filter(|row| *row < height as usize)
                else {
                    continue;
                };
                hits.push(GenUiButtonHit {
                    row: area.y.saturating_add(row as u16),
                    x_start: area.x.saturating_add(start),
                    x_end: area.x.saturating_add(end),
                    key: PressedGenuiKey {
                        slot_index,
                        vm_hash: *vm_hash.get_or_insert_with(|| {
                            TuiAssistantBubble::stable_identity_hash(
                                &bubble.text,
                                bubble.reasoning.as_ref(),
                            )
                        }),
                        epoch: BRIDGE_RESET_COUNTER.get(),
                        line_start: button.line_start,
                        x_start: button.x_start,
                        source: Arc::downgrade(lines),
                    },
                    button: button.clone(),
                });
            }
        }
    }
    hits
}

fn button_rows(slot: &VmCacheSlot, button: &GenUiButtonInfo, width: u16) -> Vec<(usize, u16, u16)> {
    let mut rows = Vec::new();
    for logical in button.line_start..button.line_end {
        let Some(entry) = slot.wrap_map.get(logical) else {
            continue;
        };
        let Some(line) = slot.lines.as_ref().and_then(|lines| lines.line(logical)) else {
            continue;
        };
        let plain = line.to_string();
        // 与选择器使用同一折行边界，避免 resize 校正帧把续行热区留在旧列。
        let starts = peri_tui_components::selection::wrap_row_byte_starts(line, &plain, width);
        for (offset, bytes) in starts.windows(2).enumerate() {
            let Some(before) = plain.get(..bytes[0]) else {
                continue;
            };
            let Some(text) = plain.get(bytes[0]..bytes[1]) else {
                continue;
            };
            let col = before.width();
            let start = (button.x_start as usize)
                .saturating_sub(col)
                .min(width as usize);
            let end = (button.x_end as usize)
                .saturating_sub(col)
                .min(text.width())
                .min(width as usize);
            if start < end {
                rows.push((
                    entry.visual_start.saturating_add(offset),
                    start as u16,
                    end as u16,
                ));
            }
        }
    }
    rows
}

pub(super) struct PressedOverlay {
    pub(super) hits: State<Arc<Vec<GenUiButtonHit>>>,
    pub(super) pressed: State<Option<PressedGenuiKey>>,
    pub(super) area: Option<Rect>,
    pub(super) strip_color: bool,
}

impl Hook for PressedOverlay {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        // resize 的校正帧可能还带着上帧的热区，等几何对齐后再应用。
        if self.area != Some(drawer.area) {
            return;
        }
        if let Some(key) = self.pressed.read().as_ref() {
            let theme = peri_theme::atoms::THEME_ATOM.state();
            draw_pressed(
                drawer.buffer_mut(),
                &self.hits.read(),
                key,
                &theme.read(),
                self.strip_color,
            );
        }
    }
}

fn draw_pressed(
    buffer: &mut Buffer,
    hits: &[GenUiButtonHit],
    key: &PressedGenuiKey,
    theme: &ThemeDefinition,
    strip_color: bool,
) {
    for hit in hits.iter().filter(|hit| key.matches(&hit.key)) {
        let rect =
            buffer
                .area
                .intersection(Rect::new(hit.x_start, hit.row, hit.x_end - hit.x_start, 1));
        if rect.is_empty() {
            continue;
        }
        // 复用已折行的屏幕热区，对含双宽字符补位的整段只覆盖样式。
        let Some(normal) = (rect.left()..rect.right())
            .map(|x| &buffer[(x, hit.row)])
            .find(|cell| cell.modifier.contains(Modifier::REVERSED))
        else {
            continue;
        };
        let style = pressed_button_style(normal.fg, theme);
        buffer.set_style(
            rect,
            if strip_color {
                peri_tui_components::style::strip_style_color(style)
            } else {
                style
            },
        );
    }
}

pub(super) fn validate_press(pressed: State<Option<PressedGenuiKey>>, hits: &[GenUiButtonHit]) {
    let stale = pressed.read().as_ref().is_some_and(|key| {
        crate::kit::mouse_router::is_occluded() || !hits.iter().any(|hit| key.matches(&hit.key))
    });
    if stale {
        *pressed.write_no_update() = None;
    }
}

pub(super) fn register_click(
    hooks: &mut Hooks,
    hits: State<Arc<Vec<GenUiButtonHit>>>,
    view_models: AtomState<ViewModelsSnapshot>,
    pressed: State<Option<PressedGenuiKey>>,
) {
    // 必须先于文本选择注册，按钮点击不能同时成为拖选起点。
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        dispatch_click(
            &event,
            &hits.read(),
            &view_models.read(),
            crate::kit::mouse_router::is_occluded(),
            |key| {
                let was_pressed = pressed.read().is_some();
                if key.is_some() || was_pressed {
                    *pressed.write() = key.cloned();
                }
            },
        )
    });
}

fn press_cancelled(event: &Event) -> bool {
    matches!(event, Event::FocusLost | Event::Resize(..))
        || matches!(event, Event::Mouse(mouse) if matches!(
            mouse.kind,
            MouseEventKind::Up(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
                // 无键移动可以清掉丢失 Up 的手势；Drag 离开按钮不取消。
                | MouseEventKind::Moved
        ))
}

fn dispatch_click(
    event: &Event,
    hits: &[GenUiButtonHit],
    snapshot: &ViewModelsSnapshot,
    occluded: bool,
    set_pressed: impl FnOnce(Option<&PressedGenuiKey>),
) -> EventResult {
    if occluded || press_cancelled(event) {
        set_pressed(None);
        return EventResult::Ignored;
    }
    let Event::Mouse(mouse) = event else {
        return EventResult::Ignored;
    };
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return EventResult::Ignored;
    }
    let Some(hit) = hits
        .iter()
        .find(|hit| hit_test(mouse.row, mouse.column, hit.row, hit.x_start, hit.x_end))
    else {
        set_pressed(None);
        return EventResult::Ignored;
    };
    let key = &hit.key;
    let matched = key.epoch == BRIDGE_RESET_COUNTER.get() && snapshot.items.get(key.slot_index).is_some_and(|vm| {
        matches!(vm, TuiRenderUnit::TuiAssistantBubble(bubble)
            if TuiAssistantBubble::stable_identity_hash(&bubble.text, bubble.reasoning.as_ref()) == key.vm_hash)
    });
    set_pressed(matched.then_some(key));
    if matched {
        dispatch_action(&hit.button);
    }
    EventResult::Consumed
}

fn dispatch_action(button: &GenUiButtonInfo) {
    match &button.action {
        GenUiAction::ToAssistant(text) => {
            let text = if text.is_empty() { &button.label } else { text };
            *INPUT_RESTORE_TEXT.get_or_init(Default::default).lock() = Some(text.clone());
            FOCUSED_ENTRY.set(None);
            AT_MENTION_ACTIVE.set(false);
            SLASH_HINT_ACTIVE.set(false);
            crate::kit::input_history::reset_history_cursor();
        }
        action @ (GenUiAction::Set(..) | GenUiAction::Reset(..)) => {
            if button.partial {
                notify(crate::i18n::tr("genui-partial-state-unsupported"));
            } else {
                let result = button
                    .block
                    .zip(button.parsed.as_ref())
                    .ok_or_else(|| "missing gen-ui block".to_owned())
                    .and_then(|(id, parsed)| view_state::apply(id, parsed, action));
                if let Err(error) = result {
                    tracing::warn!(error = %error, "gen-ui state action failed");
                    notify(crate::i18n::tr_args(
                        "genui-action-failed",
                        &[("error".into(), error.into())],
                    ));
                }
            }
        }
        GenUiAction::Run(_) => notify(crate::i18n::tr("genui-run-unsupported")),
        GenUiAction::OpenUrl(_) => notify(crate::i18n::tr("genui-open-url-unsupported")),
    }
    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
}

fn notify(message: String) {
    NOTIFICATION.set(Some(Notification {
        message,
        until: peri_time::monotonic_now() + Duration::from_secs(5),
    }));
}

#[cfg(test)]
#[path = "genui_action_test.rs"]
mod tests;
