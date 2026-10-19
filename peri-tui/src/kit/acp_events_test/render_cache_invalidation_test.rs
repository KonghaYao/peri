//! S6.4：结构性释放使 TOOL_GROUP_CACHE / 投影缓存同步失效。

use super::*;
use crate::kit::tui_render_unit::TuiToolCard;
use serial_test::serial;

fn tool(id: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: id.into(),
        tool_name: "Read".into(),
        input_summary: "path".into(),
        output_summary: "ok".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: Default::default(),
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
        tool_calls_count: 0,
    })
}

#[test]
#[serial]
fn structural_invalidation_clears_tool_group_and_history_caches() {
    crate::kit::atoms::init_atoms();
    crate::kit::acp_events::render::invalidate_structural_caches();
    let mut items: im::Vector<TuiRenderUnit> = im::Vector::from(vec![tool("t1"), tool("t2")]);
    crate::kit::acp_events::render::regroup_at_start(&mut items);
    assert!(
        crate::kit::acp_events::render::debug_cache_lengths()
            .2
            .is_some(),
        "group cache should be populated"
    );
    crate::kit::acp_events::render::invalidate_structural_caches();
    assert_eq!(
        crate::kit::acp_events::render::debug_cache_lengths(),
        (None, None, None)
    );
}
