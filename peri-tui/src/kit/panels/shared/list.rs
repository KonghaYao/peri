//! Application panel height policy; generic viewport lives in peri-tui-components.
use ratatui_kit::ratatui::layout::Rect;
/// 面板内容区行数：已测量时用实测值，首帧未测量时按终端高度预测。
///
/// `measured` 是面板自身的上一帧区域（`AreaTracker` / `use_previous_size`）；
/// 首帧为 `Rect::default()`，此时不能把面板渲染成空列表——用
/// `panel_content_rows` 的预测高度，与 `PanelOverlay` 的实际高度同源。
pub(crate) fn content_rows(measured: Option<Rect>, term_h: u16) -> u16 {
    match measured {
        // 上下边框各占 1 行（`panel_shell!` 的 TOP|BOTTOM），高度不足 3 行按未测量处理
        Some(rect) if rect.height >= 3 => rect.height - 2,
        _ => crate::kit::panel_overlay::panel_content_rows(term_h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_frame_uses_application_height_policy() {
        assert_eq!(
            content_rows(None, 30),
            crate::kit::panel_overlay::panel_content_rows(30)
        );
        assert!(content_rows(None, 30) > 0);
    }
    #[test]
    fn measured_height_wins() {
        assert_eq!(content_rows(Some(Rect::new(0, 0, 60, 18)), 30), 16);
    }
}
