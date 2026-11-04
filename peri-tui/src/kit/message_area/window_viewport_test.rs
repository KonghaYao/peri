//! 渲染缓存与结构性代次的回归用例。

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
