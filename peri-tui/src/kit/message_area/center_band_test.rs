//! 居中带鼠标命中回归（P1）：渲染列与 `CopyButtonHit` 屏幕列必须同源于 band。

use super::*;
use crate::kit::layout::{CenterBand, center_band_area};
use crate::kit::message_area::grid::GridSpec;
use ratatui_kit::ratatui::{layout::Rect, style::Style};
use ratatui_kit::test_util::render_frame;
use std::sync::{Arc, Mutex};

#[derive(Default, Props)]
struct ButtonProbeProps {
    text: &'static str,
    /// 按钮文本相对区域左缘的列偏移（模拟渲染期算出的 `btn.x_start`）。
    x_start: u16,
    sink: Arc<Mutex<Option<Rect>>>,
}

/// 测试专用探针：hook 记录 `pre_component_draw` 看到的区域，draw 在该区域
/// 内按相对偏移写按钮标记——命中公式与视觉列必须落在同一格。
struct ButtonProbe {
    text: &'static str,
    x_start: u16,
}

struct ButtonProbeHook {
    sink: Arc<Mutex<Option<Rect>>>,
}

impl Hook for ButtonProbeHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        *self.sink.lock().unwrap() = Some(drawer.area);
    }
}

impl Component for ButtonProbe {
    type Props<'a> = ButtonProbeProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self {
            text: "",
            x_start: 0,
        }
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        _updater: &mut ComponentUpdater,
    ) {
        self.text = props.text;
        self.x_start = props.x_start;
        let sink = props.sink.clone();
        hooks.use_hook(move || ButtonProbeHook { sink });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        let area = drawer.area;
        drawer.buffer_mut().set_string(
            area.x.saturating_add(self.x_start),
            area.y,
            self.text,
            Style::default(),
        );
    }
}

/// 140 列终端 left_pad=11：按钮视觉列 = band.x + 相对偏移；
/// `CopyButtonHit` 屏幕列公式 `area.x + btn.x_start` 必须命中同一格。
/// 回归时 tracker 记录 band、渲染从 x=0 开始，点击需右移 left_pad 才命中。
#[test]
fn copy_button_hit_column_matches_rendered_button_in_center_band() {
    const BTN_X_START: u16 = 62;
    let grid = GridSpec::grid_for(140);
    let band = center_band_area(Rect::new(0, 0, 140, 5), grid);
    let sink = Arc::new(Mutex::new(None));
    let buf = render_frame(
        element!(CenterBand(grid: grid) {
            ButtonProbe(text: "COPY", x_start: BTN_X_START, sink: sink.clone())
        }),
        140,
        5,
    );

    let area = sink
        .lock()
        .unwrap()
        .expect("pre_component_draw 应记录 band");
    assert_eq!(area, band, "tracker 记录的必须是带矩形");

    // 与 hits.rs 的 `CopyButtonHit` 构造公式一致：屏幕列 = area.x + btn.x_start。
    let hit = CopyButtonHit {
        row: area.y,
        x_start: area.x.saturating_add(BTN_X_START),
        x_end: area.x.saturating_add(BTN_X_START + 4),
        slot_index: 0,
        vm_hash: 0,
        logical_idx: 0,
    };

    assert_eq!(area.x, grid.left_pad);
    assert_eq!(hit.x_start, grid.left_pad + BTN_X_START);
    assert_eq!(
        buf[(hit.x_start, hit.row)].symbol(),
        "C",
        "命中列应落在按钮首字符"
    );
    assert_eq!(
        buf[(hit.x_start - 1, hit.row)].symbol(),
        " ",
        "按钮左侧应是留白而非按钮内容"
    );
}
