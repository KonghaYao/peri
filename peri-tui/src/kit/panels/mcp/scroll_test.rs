//! MCP 详情滚动回归：内容溢出时渲染不得把滚动偏移钳回 0。
//!
//! 从 `tests.rs` 拆出以保持单文件在 STD-SIZE-001 上限内；复用其挂载与前景
//! 状态恢复 helper。

use super::McpView;
use super::tests::{ForegroundGuard, mount_panel_frames};
use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, MCP_SERVERS, McpServerSummary, McpToolSummary, POPUP_KIND};
use ratatui_kit::prelude::ScrollViewState;
use ratatui_kit::ratatui::layout::Position;
use serial_test::serial;

/// 旧渲染把 `Text` 直接交给 ScrollView，框架按 `Constraint::Percentage(100)`
/// 推导内容高度 = 视口高度，渲染期 `saturating_sub` 把任意偏移钳回 0——滚轮
/// 永远无效。修复后内容高度取自实际行数，非零偏移能跨帧保留。
#[test]
#[serial]
fn detail_scroll_offset_survives_render_when_content_overflows() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    // 80 个工具远超 80×24 面板的详情视口，确保内容真实溢出。
    *MCP_SERVERS.state().write() = vec![McpServerSummary {
        name: "alpha".into(),
        tools_count: 80,
        tools: (0..80)
            .map(|index| McpToolSummary {
                name: format!("tool{index:02}"),
                description: None,
            })
            .collect(),
        ..McpServerSummary::default()
    }];

    mount_panel_frames(3, |frame, panel| match frame {
        0 => {
            // 第一帧进入详情（本帧 visitor 先于渲染，下一帧起用详情内容布局）。
            *panel.view.write() = McpView::Detail;
            *panel.detail_key.write() = Some("alpha".into());
        }
        1 => {
            assert_eq!(panel.sv.read().offset().y, 0, "进入详情从顶部开始");
            *panel.sv.write() = ScrollViewState::with_offset(Position::new(0, 5));
        }
        _ => assert_eq!(
            panel.sv.read().offset().y,
            5,
            "内容溢出时渲染不得把滚动偏移钳回 0"
        ),
    });
}
