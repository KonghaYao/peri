#[path = "text_layout.rs"]
mod text_layout;
use self::text_layout as plain;

#[cfg(test)]
#[path = "subagent_detail_cache_test.rs"]
mod tests;

#[cfg(test)]
use crate::kit::message_area::grid::GridSpec;
#[cfg(test)]
use crate::kit::tui_render_unit::FoldKey;
#[cfg(test)]
use crate::kit::tui_render_unit::TuiSubAgentGroup;
#[cfg(test)]
use peri_theme::theme::ThemeDefinition;
#[cfg(test)]
use ratatui_kit::prelude::ComponentDrawer;
#[cfg(test)]
use ratatui_kit::ratatui::layout::Rect;
#[cfg(test)]
use ratatui_kit::ratatui::text::Line;
#[cfg(test)]
use std::sync::Arc;

#[path = "subagent_detail_cache/detail_render_cache.rs"]
mod detail_render_cache;
use self::detail_render_cache as detail_cache;
pub(super) use detail_cache::DetailRenderCache;

#[path = "subagent_detail_cache/detail_artifacts.rs"]
mod detail_artifacts;
pub(super) use detail_artifacts::{
    DetailArtifacts, apply_click as apply_artifact_click, collect as collect_artifacts,
    resolve_click as resolve_artifact_click,
};
#[cfg(test)]
use detail_cache::projected;
