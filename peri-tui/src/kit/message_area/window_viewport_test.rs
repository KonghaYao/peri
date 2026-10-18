//! S5.6：真实视口发布窗口身份，宽度变化后仍按身份重定位。

use super::grid::GridSpec;
use super::transcript::Transcript;
use super::*;
use crate::kit::atoms::{TranscriptPublication, ViewModelsSnapshot};
use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};
use peri_theme::atoms::THEME_ATOM;

fn context() -> EntryRenderContext {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

fn snapshot(count: usize) -> ViewModelsSnapshot {
    ViewModelsSnapshot {
        generation: 1,
        items: (0..count)
            .map(|slot| {
                TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("row-{slot} 中文🙂")))
            })
            .collect(),
    }
}

#[test]
#[serial_test::serial]
fn real_viewport_publishes_window_identity_and_survives_width_change() {
    use crate::kit::history_window::{
        WindowUnitMapping, clear_window_viewport, publish_window_unit_mappings,
    };

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(true);
    crate::kit::atoms::HISTORY_WINDOW_GENERATION.set(3);
    clear_window_viewport();
    let mut snapshot = snapshot(1);
    snapshot.generation = 1;
    let token = crate::kit::message_area::window_identity_token(&snapshot.items[0]);
    publish_window_unit_mappings(vec![WindowUnitMapping {
        token,
        entry: crate::kit::history_window::PersistentEntryKey::own("s1:m1"),
    }]);
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let context = context();
    let grid = GridSpec::grid_for(80);
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        1,
        0,
        0,
        2,
        0,
    );
    let viewport = crate::kit::atoms::WINDOW_VIEWPORT
        .state()
        .read()
        .clone()
        .expect("viewport published");
    assert_eq!(viewport.window_generation, 3);
    assert_eq!(
        viewport
            .anchor
            .as_ref()
            .map(|anchor| &anchor.entry.entry_id),
        Some(&"s1:m1".to_string())
    );

    // 宽度变化后仍按同一持久身份发布，不按行号补偿。
    let narrow = GridSpec::grid_for(40);
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        narrow,
        narrow.line_width(),
        context,
        1,
        0,
        0,
        2,
        0,
    );
    let viewport = crate::kit::atoms::WINDOW_VIEWPORT
        .state()
        .read()
        .clone()
        .expect("viewport republished");
    assert_eq!(
        viewport
            .anchor
            .as_ref()
            .map(|anchor| &anchor.entry.entry_id),
        Some(&"s1:m1".to_string())
    );
    crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
    clear_window_viewport();
}

#[test]
#[serial_test::serial]
fn structural_epoch_with_empty_snapshot_reaches_zero_retained_bytes() {
    crate::kit::atoms::init_atoms();
    let old_epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get();
    let mut snapshot = snapshot(1);
    snapshot.generation = 1;
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let context = context();
    let grid = GridSpec::grid_for(80);
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        1,
        0,
        0,
        2,
        0,
    );
    assert!(transcript.retained_bytes() > 0);

    crate::kit::atoms::STRUCTURAL_EPOCH.set(old_epoch + 1);
    snapshot.items.clear();
    snapshot.generation = 2;
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        2,
        0,
        0,
        2,
        0,
    );
    assert_eq!(transcript.retained_bytes(), 0);
    assert_eq!(crate::kit::atoms::TUI_RENDER_CACHE_BYTES.get(), 0);
    assert!(caches.is_empty());
    crate::kit::atoms::STRUCTURAL_EPOCH.set(old_epoch);
    crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(old_epoch);
}

#[test]
#[serial_test::serial]
fn structural_epoch_drops_old_slot_lines_and_uses_8mib_to_6mib_budget() {
    crate::kit::atoms::init_atoms();
    crate::kit::acp_events::render::invalidate_structural_caches();
    let old_epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get();
    let mut snapshot = snapshot(1);
    snapshot.generation = 1;
    let mut transcript = Transcript::default();
    let mut caches = Vec::new();
    let context = context();
    let grid = GridSpec::grid_for(80);
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context.clone(),
        1,
        0,
        0,
        2,
        0,
    );
    let old_lines =
        std::sync::Arc::downgrade(caches[0].lines.as_ref().expect("rendered slot lines"));

    crate::kit::atoms::STRUCTURAL_EPOCH.set(old_epoch + 1);
    transcript.prepare(
        &snapshot,
        &TranscriptPublication::default(),
        &mut caches,
        grid,
        grid.line_width(),
        context,
        2,
        0,
        0,
        2,
        0,
    );
    assert!(
        old_lines.upgrade().is_none(),
        "structural epoch must drop old SlotLines"
    );
    assert_eq!(
        crate::kit::atoms::RENDER_ACHIEVED_EPOCH.get(),
        old_epoch + 1
    );
    assert_eq!(
        crate::kit::atoms::TUI_RENDER_CACHE_BYTES.get(),
        transcript.retained_bytes()
    );
    assert_eq!(
        Transcript::render_cache_budgets(),
        (8 * 1024 * 1024, 6 * 1024 * 1024)
    );
    crate::kit::atoms::STRUCTURAL_EPOCH.set(old_epoch);
    crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(old_epoch);
}
