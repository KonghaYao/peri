//! 根级 `NO_COLOR` 剥离：用 ratatui-kit `test_util` 渲染最小根组件，确认整帧
//! （含模拟面板区域）一次去色、modifier 与文本保留。

use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::test_util::render_frame;
use serial_test::serial;

use super::NoColorFrameHook;
use crate::kit::atoms::TERMINAL_CAPS;
use crate::kit::terminal_caps::TerminalCaps;

/// 测试期间切换能力位；Drop 还原（caps 是进程级 atom，避免污染其它用例）。
struct CapsGuard(TerminalCaps);

impl CapsGuard {
    fn set(color: bool) -> Self {
        let saved = TERMINAL_CAPS.get();
        TERMINAL_CAPS.set(TerminalCaps { color, ..saved });
        Self(saved)
    }
}

impl Drop for CapsGuard {
    fn drop(&mut self) {
        TERMINAL_CAPS.set(self.0);
    }
}

/// 模拟同一帧的两个 surface：第 0 行主区（REVERSED 选区）、第 1 行
/// SubAgent 面板（带 bg/underline color）。
struct TwoSurfaceProbe;

impl Component for TwoSurfaceProbe {
    type Props<'a> = NoProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        _updater: &mut ComponentUpdater,
    ) {
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        let area = drawer.area;
        let buffer = drawer.buffer_mut();
        buffer.set_stringn(
            area.x,
            area.y,
            "main",
            area.width as usize,
            Style::default()
                .fg(Color::Rgb(200, 0, 0))
                .bg(Color::Rgb(0, 0, 80))
                .add_modifier(Modifier::REVERSED),
        );
        if area.height > 1 {
            buffer.set_stringn(
                area.x,
                area.y + 1,
                "panel",
                area.width as usize,
                Style::default()
                    .fg(Color::Rgb(0, 200, 0))
                    .bg(Color::Rgb(20, 20, 20))
                    .underline_color(Color::Rgb(1, 2, 3))
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
}

/// 最小根组件：注册与 `AppShell` 相同的根 hook，再绘制子组件。
#[component]
fn NoColorRoot(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _hook = hooks.use_hook(NoColorFrameHook::new);
    element!(TwoSurfaceProbe())
}

#[test]
#[serial]
fn root_hook_strips_colors_from_the_whole_frame() {
    let _guard = CapsGuard::set(false);
    let buffer = render_frame(element!(NoColorRoot()), 12, 2);

    assert_eq!(buffer[(0, 0)].symbol(), "m", "主区文本保留");
    assert_eq!(buffer[(0, 1)].symbol(), "p", "面板文本保留");
    for y in 0..2 {
        for x in 0..12 {
            let cell = &buffer[(x, y)];
            assert_eq!(cell.fg, Color::Reset, "fg cleared at ({x},{y})");
            assert_eq!(cell.bg, Color::Reset, "bg cleared at ({x},{y})");
            assert_eq!(
                cell.underline_color,
                Color::Reset,
                "underline color cleared at ({x},{y})"
            );
        }
    }
    assert!(
        buffer[(0, 0)].modifier.contains(Modifier::REVERSED),
        "主区选区 REVERSED 保留"
    );
    assert!(
        buffer[(0, 1)].modifier.contains(Modifier::BOLD),
        "面板 BOLD 保留"
    );
}

#[test]
#[serial]
fn root_hook_keeps_colors_when_capability_allows_them() {
    let _guard = CapsGuard::set(true);
    let buffer = render_frame(element!(NoColorRoot()), 12, 2);

    assert_eq!(buffer[(0, 0)].fg, Color::Rgb(200, 0, 0));
    assert_eq!(buffer[(0, 0)].bg, Color::Rgb(0, 0, 80));
    assert_eq!(buffer[(0, 1)].fg, Color::Rgb(0, 200, 0));
    assert_eq!(buffer[(0, 1)].underline_color, Color::Rgb(1, 2, 3));
}
