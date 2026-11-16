use std::sync::Arc;
use std::time::Duration;

use ratatui_kit::crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use super::selection::SlotIndex;
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::{
    AT_MENTION_ACTIVE, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, INPUT_RESTORE_TEXT, NOTIFICATION,
    Notification, RENDER_HEARTBEAT, SLASH_HINT_ACTIVE, ViewModelsSnapshot,
};
use crate::kit::genui::{GenUiAction, GenUiButtonInfo, view_state};
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};

pub(super) struct GenUiButtonHit {
    row: u16,
    x_start: u16,
    x_end: u16,
    slot_index: usize,
    vm_hash: u64,
    epoch: u64,
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
        let slot_start = index.slot_visual_start(slot_index).unwrap_or(0);
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
                    slot_index,
                    vm_hash: TuiAssistantBubble::stable_identity_hash(
                        &bubble.text,
                        bubble.reasoning.as_ref(),
                    ),
                    epoch: BRIDGE_RESET_COUNTER.get(),
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
        let starts = super::selection::wrap_byte_starts(line, &plain, width);
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

pub(super) fn register_click(
    hooks: &mut Hooks,
    hits: State<Arc<Vec<GenUiButtonHit>>>,
    view_models: AtomState<ViewModelsSnapshot>,
) {
    // 必须先于文本选择注册，按钮点击不能同时成为拖选起点。
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        dispatch_click(
            &event,
            &hits.read(),
            &view_models.read(),
            crate::kit::mouse_router::is_occluded(),
        )
    });
}

fn dispatch_click(
    event: &Event,
    hits: &[GenUiButtonHit],
    snapshot: &ViewModelsSnapshot,
    occluded: bool,
) -> EventResult {
    let Event::Mouse(mouse) = event else {
        return EventResult::Ignored;
    };
    if occluded || mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return EventResult::Ignored;
    }
    let Some(hit) = hits.iter().find(|hit| {
        mouse.row == hit.row && mouse.column >= hit.x_start && mouse.column < hit.x_end
    }) else {
        return EventResult::Ignored;
    };
    let matched = hit.epoch == BRIDGE_RESET_COUNTER.get() && snapshot.items.get(hit.slot_index).is_some_and(|vm| {
        matches!(vm, TuiRenderUnit::TuiAssistantBubble(bubble)
            if TuiAssistantBubble::stable_identity_hash(&bubble.text, bubble.reasoning.as_ref()) == hit.vm_hash)
    });
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
