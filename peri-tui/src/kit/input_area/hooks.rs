use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;
use unicode_width::UnicodeWidthChar;

/// 在 post_component_draw 时修复 CJK 续接 cell 的 diff 不可见性。
///
/// ratatui `set_stringn` 对双宽字符的续接 cell 始终 reset 到 `Cell::EMPTY`
/// (bg=Color::Reset, 无 modifier)。两帧续接 cell 相同 → diff 跳过 → 终端保留
/// 主 cell bg 的视觉扩展（光标白色残影）。
///
/// 此 hook 在每帧渲染后将续接 cell 标记 `AlwaysUpdate`，强制 diff 发送 SGR，
/// 但 **不修改 bg/fg 值**——视觉上完全透明，无底色。
pub(super) struct CjkGhostFix;

impl Hook for CjkGhostFix {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        use ratatui::buffer::CellDiffOption;
        let area = drawer.area;
        let buf = drawer.buffer_mut();
        let right = area.right();
        let bottom = area.bottom();
        for y in area.y..bottom {
            let mut x = area.x;
            while x < right {
                let w = {
                    let symbol = buf[(x, y)].symbol();
                    if symbol.is_empty() {
                        0
                    } else {
                        symbol.chars().next().and_then(|c| c.width()).unwrap_or(0) as u16
                    }
                };
                if w > 1 {
                    for dx in 1..w {
                        let cx = x + dx;
                        if cx < right {
                            buf[(cx, y)].diff_option = CellDiffOption::AlwaysUpdate;
                        }
                    }
                    x += w;
                } else {
                    x += 1;
                }
            }
        }
    }
}

/// 追踪 composer 段落区域，供鼠标点击→光标定位使用。
/// 仿照 MsgAreaTracker 模式：rect 是值类型，每帧 pre_component_draw 更新后在
/// handler 注册前取出副本传给闭包。
pub(super) struct AreaTracker {
    pub(super) rect: Option<Rect>,
}

impl Hook for AreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}
