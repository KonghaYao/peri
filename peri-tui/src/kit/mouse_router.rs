//! 鼠标事件遮挡集中判定——背景组件统一让路入口。
//!
//! 背景组件（消息区/输入区/状态栏）原先各自维护"是否被弹窗/面板遮挡"的
//! 手动检查，且判定不一致（消息区/输入区漏 ACTIVE_PANEL）。本模块集中
//! 定义遮挡集。见 spec/issues/2026-08-01-tui-mouse-multi-layer-conflict.md（方案 A1）。
//!
//! 滚轮例外：居中弹窗（HITL 授权等）与面板都只覆盖屏幕一部分，遮挡区域外
//! 的消息区仍可见——`occludes_scroll(x, y)` 按鼠标坐标判定滚轮是否遮挡
//! （弹窗矩形/面板区域外放行给消息区滚动）；点击类事件不受影响，仍由
//! `is_occluded` 全遮挡。

use crate::kit::atoms::{ACTIVE_PANEL, PANEL_AREA, POPUP_AREA, POPUP_KIND};
use ratatui_kit::ratatui::layout::Rect;

/// 任何前景模态层（弹窗/面板）激活时返回 true，背景组件应让路（返回 `EventResult::Ignored`）。
///
/// 遮挡集集中定义：新增浮层类型只需改这里，背景组件零改动。
pub fn is_occluded() -> bool {
    POPUP_KIND.state().read().is_some() || ACTIVE_PANEL.state().read().is_some()
}

/// 仅弹窗层（不含面板）——底栏 D4：面板打开时仍可点 BgTask 行，弹窗仍挡。
pub fn is_popup_active() -> bool {
    POPUP_KIND.state().read().is_some()
}

/// 底栏 BgTask 行是否允许进入路由（D4）。
///
/// - 弹窗打开 → 一律不允许。
/// - 无弹窗且命中底栏行 → 允许（即使 `ACTIVE_PANEL` 已开）。
/// - 未命中底栏行 → 不允许（handler 应 `Ignored`，不吞消息区/输入）。
pub fn bg_bar_click_allowed(hit_on_bg_row: bool) -> bool {
    if is_popup_active() {
        return false;
    }
    hit_on_bg_row
}

/// 弹窗或面板打开时，滚轮事件在该屏幕坐标是否仍应被遮挡。
///
/// 与 `is_occluded` 的关系：调用方在 `is_occluded()` 为 true 时用它进一步
/// 区分「浮层内/外」，只放行遮挡区域外的滚轮：
/// - 弹窗打开且鼠标落在 `POPUP_AREA` 矩形内 → 遮挡（滚轮归弹窗/无效）
/// - 面板打开且鼠标落在 `PANEL_AREA` 矩形内 → 遮挡（滚轮归面板滚轮仲裁）
/// - 鼠标在弹窗/面板区域外 → 放行（消息区滚轮生效）
/// - 弹窗/面板矩形未知（尚未渲染一帧 / 自定位小层如 ModelQuickSwitch 未登记）
///   → 保守遮挡
pub fn occludes_scroll(x: u16, y: u16) -> bool {
    let popup_occludes = POPUP_KIND.state().read().is_some()
        && match *POPUP_AREA.state().read() {
            None => true,
            Some(rect) => rect_contains(x, y, rect),
        };
    if popup_occludes {
        return true;
    }
    ACTIVE_PANEL.state().read().is_some()
        && match *PANEL_AREA.state().read() {
            None => true,
            Some(rect) => rect_contains(x, y, rect),
        }
}

fn rect_contains(x: u16, y: u16, rect: Rect) -> bool {
    let right = rect.x.saturating_add(rect.width);
    let bottom = rect.y.saturating_add(rect.height);
    x >= rect.x && x < right && y >= rect.y && y < bottom
}

#[cfg(test)]
mod tests {
    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms::{ACTIVE_PANEL, PANEL_AREA, POPUP_AREA, POPUP_KIND, PopupKind};
    use ratatui_kit::ratatui::layout::Rect;
    use serial_test::serial;

    /// 清理全局 atom，防止测试间污染（仿 input_area_test reset 模式）。
    fn reset() {
        *POPUP_KIND.state().write() = None;
        *ACTIVE_PANEL.state().write() = None;
        *POPUP_AREA.state().write() = None;
        *PANEL_AREA.state().write() = None;
    }

    #[test]
    #[serial]
    fn no_foreground_not_occluded() {
        reset();
        assert!(!super::is_occluded());
    }

    #[test]
    #[serial]
    fn popup_occludes() {
        reset();
        *POPUP_KIND.state().write() = Some(PopupKind::Confirm);
        assert!(super::is_occluded());
    }

    #[test]
    #[serial]
    fn panel_occludes() {
        reset();
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Config);
        assert!(super::is_occluded());
    }

    #[test]
    #[serial]
    fn test_is_occluded_unchanged_popup_and_panel() {
        reset();
        assert!(!super::is_occluded());
        *POPUP_KIND.state().write() = Some(PopupKind::Confirm);
        assert!(super::is_occluded());
        reset();
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Config);
        assert!(super::is_occluded());
        assert!(!super::is_popup_active());
        assert!(super::bg_bar_click_allowed(true));
    }

    #[test]
    #[serial]
    fn test_bg_bar_click_blocked_when_popup() {
        reset();
        *POPUP_KIND.state().write() = Some(PopupKind::Confirm);
        assert!(!super::bg_bar_click_allowed(true));
        assert!(!super::bg_bar_click_allowed(false));
    }

    #[test]
    #[serial]
    fn test_bg_bar_click_allowed_panel_open_hit_row() {
        reset();
        *ACTIVE_PANEL.state().write() = Some(PanelKind::SubAgentDetail);
        assert!(super::is_occluded());
        assert!(super::bg_bar_click_allowed(true));
    }

    #[test]
    #[serial]
    fn test_bg_bar_click_miss_outside_row_ignored() {
        reset();
        assert!(super::bg_bar_click_allowed(true));
        assert!(!super::bg_bar_click_allowed(false));
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Workflow);
        assert!(!super::bg_bar_click_allowed(false));
    }

    // ── occludes_scroll：弹窗外滚轮放行 ─────────────────────────────────

    #[test]
    #[serial]
    fn scroll_no_popup_or_panel_not_occluded() {
        reset();
        assert!(!super::occludes_scroll(1, 1));
    }

    #[test]
    #[serial]
    fn scroll_inside_popup_rect_occluded() {
        reset();
        *POPUP_KIND.state().write() = Some(PopupKind::Hitl);
        *POPUP_AREA.state().write() = Some(Rect::new(10, 5, 60, 20));
        // 矩形内部（含边界：x=10..69, y=5..24）
        assert!(super::occludes_scroll(10, 5));
        assert!(super::occludes_scroll(69, 24));
        // 边界外 1 格
        assert!(!super::occludes_scroll(9, 5));
        assert!(!super::occludes_scroll(70, 24));
        assert!(!super::occludes_scroll(10, 4));
        assert!(!super::occludes_scroll(10, 25));
    }

    #[test]
    #[serial]
    fn scroll_outside_popup_rect_passthrough() {
        reset();
        *POPUP_KIND.state().write() = Some(PopupKind::Hitl);
        *POPUP_AREA.state().write() = Some(Rect::new(10, 5, 60, 20));
        assert!(!super::occludes_scroll(0, 0));
        assert!(!super::occludes_scroll(100, 30));
    }

    #[test]
    #[serial]
    fn scroll_popup_without_registered_rect_occluded() {
        reset();
        // ModelQuickSwitch 等自定位小层不登记 POPUP_AREA → 保守遮挡
        *POPUP_KIND.state().write() = Some(PopupKind::ModelQuickSwitch);
        assert!(super::occludes_scroll(0, 0));
        assert!(super::occludes_scroll(100, 30));
    }

    #[test]
    #[serial]
    fn scroll_panel_open_occluded_without_area() {
        reset();
        // 面板打开但尚未登记区域（首帧前）→ 保守遮挡
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Config);
        assert!(super::occludes_scroll(0, 0));
        assert!(super::occludes_scroll(100, 30));
    }

    #[test]
    #[serial]
    fn scroll_panel_inside_area_occluded() {
        reset();
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Model);
        *PANEL_AREA.state().write() = Some(Rect::new(0, 20, 120, 15));
        // 面板区域内（含边界）
        assert!(super::occludes_scroll(0, 20));
        assert!(super::occludes_scroll(119, 34));
        // 面板区域外（上方消息区）→ 放行
        assert!(!super::occludes_scroll(0, 19));
        assert!(!super::occludes_scroll(60, 10));
        // 下方（输入区/状态栏）
        assert!(!super::occludes_scroll(60, 35));
    }

    #[test]
    #[serial]
    fn scroll_panel_outside_area_passthrough() {
        reset();
        *ACTIVE_PANEL.state().write() = Some(PanelKind::ThreadBrowser);
        *PANEL_AREA.state().write() = Some(Rect::new(0, 20, 120, 15));
        assert!(!super::occludes_scroll(60, 5));
        assert!(!super::occludes_scroll(60, 40));
    }

    #[test]
    #[serial]
    fn scroll_popup_and_panel_any_occludes() {
        reset();
        // 弹窗+面板同时打开：弹窗矩形外但面板区域内 → 仍遮挡（面板滚轮归仲裁）
        *POPUP_KIND.state().write() = Some(PopupKind::Hitl);
        *POPUP_AREA.state().write() = Some(Rect::new(30, 5, 60, 10));
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Model);
        *PANEL_AREA.state().write() = Some(Rect::new(0, 20, 120, 15));
        assert!(super::occludes_scroll(60, 25)); // 面板内
        assert!(!super::occludes_scroll(60, 15)); // 弹窗外（y>14）且面板外（y<20）
    }
}
