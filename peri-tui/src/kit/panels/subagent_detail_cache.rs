use std::sync::Arc;

use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::text::Line;

use crate::kit::markdown::MarkdownRenderCache;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render::vm_to_lines_cached;
use crate::kit::tui_render_unit::TuiSubAgentGroup;

#[derive(Default)]
pub(super) struct DetailRenderCache {
    instance_id: String,
    width: u16,
    theme: Option<Arc<ThemeDefinition>>,
    language: u64,
    slots: Vec<DetailSlot>,
}

#[derive(Default)]
struct DetailSlot {
    hash: Option<u64>,
    frame: u64,
    markdown: MarkdownRenderCache,
    lines: Vec<Line<'static>>,
}

impl DetailRenderCache {
    pub(super) fn render(
        &mut self,
        group: &TuiSubAgentGroup,
        grid: &GridSpec,
        theme: Arc<ThemeDefinition>,
        language: u64,
    ) -> Vec<Line<'static>> {
        let width = grid.line_width();
        if self.instance_id != group.instance_id
            || self.width != width
            || self.language != language
            || !self
                .theme
                .as_ref()
                .is_some_and(|previous| Arc::ptr_eq(previous, &theme))
        {
            self.slots.clear();
            self.instance_id.clone_from(&group.instance_id);
            self.width = width;
            self.language = language;
            self.theme = Some(theme);
        }
        self.slots
            .resize_with(group.view_models.len(), DetailSlot::default);
        let frame = peri_time::now_wall()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64 / 100)
            .unwrap_or(0);
        let mut lines = Vec::new();
        for (slot, vm) in self.slots.iter_mut().zip(group.view_models.iter()) {
            let period = vm.animation_period_frames();
            if slot.hash != Some(vm.content_hash())
                || (period != 0 && slot.frame / period != frame / period)
            {
                slot.lines = vm_to_lines_cached(vm, grid, &mut slot.markdown, false).0;
                slot.hash = Some(vm.content_hash());
                slot.frame = frame;
            }
            lines.extend(slot.lines.iter().cloned());
        }
        lines
    }
}

#[cfg(test)]
#[path = "subagent_detail_cache_test.rs"]
mod tests;
