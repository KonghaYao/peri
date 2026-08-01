//! 鼠标事件遮挡集中判定——背景组件统一让路入口。
//!
//! 背景组件（消息区/输入区/状态栏）原先各自维护"是否被弹窗/面板遮挡"的
//! 手动检查，且判定不一致（消息区/输入区漏 ACTIVE_PANEL）。本模块集中
//! 定义遮挡集。见 spec/issues/2026-08-01-tui-mouse-multi-layer-conflict.md（方案 A1）。

use crate::kit::atoms::{ACTIVE_PANEL, POPUP_KIND};

/// 任何前景模态层（弹窗/面板）激活时返回 true，背景组件应让路（返回 `EventResult::Ignored`）。
///
/// 遮挡集集中定义：新增浮层类型只需改这里，背景组件零改动。
pub fn is_occluded() -> bool {
    POPUP_KIND.state().read().is_some() || ACTIVE_PANEL.state().read().is_some()
}

#[cfg(test)]
mod tests {
    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms::{ACTIVE_PANEL, POPUP_KIND, PopupKind};
    use serial_test::serial;

    /// 清理全局 atom，防止测试间污染（仿 input_area_test reset 模式）。
    fn reset() {
        *POPUP_KIND.state().write() = None;
        *ACTIVE_PANEL.state().write() = None;
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
}
