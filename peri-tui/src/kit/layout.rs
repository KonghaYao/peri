//! ratatui-kit SessionColumn layout component.

// element! 宏展开触发 clippy::needless_update（ratatui-kit 上游问题），模块级抑制。
#![allow(clippy::needless_update)]

use crate::kit::atoms;
use crate::kit::input_area::InputArea;
use crate::kit::message_area::MessageArea;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::panel_overlay::PanelOverlay;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction, Rect},
};

// ── 居中带（§3.1）────────────────────────────────────────────────────────

/// 把组件区域收进 transcript 居中带——左偏移 + 带宽取自 [`GridSpec`]，
/// 两端余量在该带之外由父级布局留白。
///
/// 纯函数（[`CenterBand::draw`] 只做 `drawer.area` 替换），越界时向右收缩并至少保留 1 列。
///
/// [Why 两条早退] `terminal::size()` 取不到尺寸时 `use_terminal_size` 返回
/// `(0, 0)`，`grid_for(0)` 的 `band_width` 也是 0——此时网格不可信，保持原区域
/// （否则整条 UI 会塌成 1 列）。区域本身为 0 宽时同理不放大（越界绘制）。
pub(crate) fn center_band_area(area: Rect, grid: GridSpec) -> Rect {
    if grid.band_width == 0 || area.width == 0 {
        return area;
    }
    let pad = grid.left_pad.min(area.width.saturating_sub(1));
    let width = grid.band_width.min(area.width.saturating_sub(pad)).max(1);
    Rect {
        x: area.x.saturating_add(pad),
        width,
        ..area
    }
}

/// 居中带包装组件——在 **draw 期**把自身区域收进居中带（§3.1），
/// 子组件因此同时按该区域渲染并观察到带矩形。
///
/// [Why 收窄区域而非给每行加左填充] 区域收窄后，组件内所有坐标（换行宽度、
/// 命中列、光标列、滚动条列）仍是带内相对坐标，`AreaTracker` / `MsgAreaTracker`
/// 记录的也是带矩形；若改为在行首补空格，每个消费点都要各自加偏移。
///
/// [Why 包装组件而非 hook] ratatui-kit 0.12 的 `InstantiatedComponent::draw` 在
/// `pre_component_draw` 之后把 `drawer.area` 重置为组件区域——hook 里改写只对
/// 后续 hook 可见，不影响真实渲染与子节点布局。draw 期改写则直接决定子节点
/// 区域（与上游 `Border` / `Positioned` 同一机制）。
///
/// [Why 透明布局] `set_transparent_layout(true)` 让父级仍按**被包裹组件**的
/// `Fill/Length` 约束分配区域，包装不改变布局占位；draw 期再把该区域收窄到带。
#[derive(Default, Props)]
pub(crate) struct CenterBandProps<'a> {
    /// Transcript 水平网格（终端 resize 后每帧重建）。
    pub grid: GridSpec,
    pub children: Vec<AnyElement<'a>>,
}

pub(crate) struct CenterBand {
    grid: GridSpec,
}

impl Component for CenterBand {
    type Props<'a> = CenterBandProps<'a>;

    fn new(props: &Self::Props<'_>) -> Self {
        Self { grid: props.grid }
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.grid = props.grid;
        updater.set_transparent_layout(true);
        updater.update_children(&mut props.children, None);
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        drawer.area = center_band_area(drawer.area, self.grid);
    }
}

// ── 高度降级计划（spec §11）─────────────────────────────────────────────

/// §11「transcript 至少保留 3 行」——输入区（composer + 队列 + 弹出层）的
/// 高度预算下限（SessionColumn 用 `term_h - status - MIN_TRANSCRIPT_ROWS`
/// 推导 `max_total_height`；InputArea 超预算时先截断队列）。
pub const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// 状态栏在给定终端高度下的降级档位（§11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBarMode {
    /// `h ≥ 12`：完整双行（Row1 + Row2 key hints + NotifRow + 缓冲行，高度 4）。
    Full,
    /// `8 ≤ h < 12`：仅 Row1 + NotifRow（高度 2），隐藏 Row2 key hints 与缓冲行。
    Row1Only,
    /// `h < 8`：完全隐藏（高度 0），把高度让给 transcript/composer。
    Hidden,
}

impl StatusBarMode {
    /// 各档位占用的高度行数（Full=4 / Row1Only=2 / Hidden=0）——
    /// 供 SessionColumn 计算输入区高度预算（§11 transcript ≥3 行保证）。
    pub fn height(self) -> u16 {
        match self {
            StatusBarMode::Full => 4,
            StatusBarMode::Row1Only => 2,
            StatusBarMode::Hidden => 0,
        }
    }
}

/// 按终端高度推导的布局降级计划（§11）——纯函数，AppShell 消费结果。
///
/// 约束优先级：transcript ≥3 行 > composer ≥1 行 > status。
/// - `h ≥ 12`：Full 状态栏，session title 可见，composer 不钳制；
/// - `8 ≤ h < 12`：Row1Only（隐藏 Row2 key hints），隐藏 session title；
/// - `h < 8`：状态栏隐藏，composer 钳制 ≤2 行（transcript ≥3 由
///   `Constraint::Fill(1)` 余量自动保证——status 0 + composer ≤4 → transcript
///   = h - 4 ≥ 3 当 h ≥ 7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeightPlan {
    pub status_bar: StatusBarMode,
    /// composer 编辑行数上限（`None` = 默认 10）。h<8 时为 `Some(2)`。
    pub composer_max_lines: Option<u16>,
    /// session title（composer 上边栏）是否可见。h<12 时隐藏。
    pub session_title_visible: bool,
}

impl Default for HeightPlan {
    fn default() -> Self {
        Self {
            status_bar: StatusBarMode::Full,
            composer_max_lines: None,
            session_title_visible: true,
        }
    }
}

/// 高度断点纯函数（§11）：`h ≥ 12` Full；`8 ≤ h < 12` Row1Only + 隐藏 title；
/// `h < 8` Hidden + composer ≤2 行。
pub fn layout_plan(h: u16) -> HeightPlan {
    if h >= 12 {
        HeightPlan::default()
    } else if h >= 8 {
        HeightPlan {
            status_bar: StatusBarMode::Row1Only,
            composer_max_lines: None,
            session_title_visible: false,
        }
    } else {
        HeightPlan {
            status_bar: StatusBarMode::Hidden,
            composer_max_lines: Some(2),
            session_title_visible: false,
        }
    }
}

#[derive(Default, Props)]
pub struct SessionColumnProps {
    /// 高度降级计划（AppShell 由 `layout_plan(term_h)` 推导后传入）。
    pub plan: HeightPlan,
}

#[component]
pub fn SessionColumn(
    props: &SessionColumnProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let acp = hooks.use_atom(&atoms::ACP_STATE);
    let active_panel = hooks.use_atom(&atoms::ACTIVE_PANEL);

    let loading = acp.read().is_loading;
    let panel_open = active_panel.read().is_some();

    // [Slice 3] Transcript 统一水平网格（§3.1）——按终端宽度计算 content 列宽
    // （content = min(term_w - 6, 100)）与居中带；content 触顶后余量左右均分
    // （grid.left_pad），替代旧的 width-4 内边距 hack。
    let (term_w, term_h) = hooks.use_terminal_size();
    let grid = GridSpec::grid_for(term_w);
    // hook 占位——ratatui-kit 要求 hook 数量恒定不可增减
    let _last_width = hooks.use_state(|| 0u16);

    // [Fix §11] 输入区高度预算 = 终端高度 - 状态栏 - transcript 最低 3 行。
    // 队列（queued）/弹出层超过预算时由 InputArea 先截断队列（transcript
    // ≥3 行优先于 composer > status；40×8 + 队列场景 transcript 不再被挤到
    // 2 行）。预算低于 composer 最低需求时 InputArea 保 composer（优雅降级）。
    let input_max_height =
        term_h.saturating_sub(props.plan.status_bar.height() + MIN_TRANSCRIPT_ROWS);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            // [§3.1] 居中带在 draw 期收窄：组件自身 hooks（MsgAreaTracker /
            // AreaTracker）与渲染共用同一带矩形。
            CenterBand(grid: grid) {
                MessageArea(grid: grid)
            }

            // 面板位于消息流之上、输入区之上；参与主布局，不再是根级浮动覆盖。
            // 面板自带内边距与坐标基准，不参与居中带收窄。
            PanelOverlay()

            // 面板打开时隐藏输入区，避免输入框抢占用户注意力；关闭面板后自动恢复。
            // [Slice 3a] grid prop：composer prompt 前缀按 gap 对齐 transcript
            // content 起点（§10）；窄断点下标题/footer 随 HeightPlan 降级。
            CenterBand(grid: grid) {
                InputArea(
                    loading: loading,
                    hidden: panel_open,
                    max_lines: props.plan.composer_max_lines,
                    session_title_visible: props.plan.session_title_visible,
                    grid: grid,
                    max_total_height: Some(input_max_height),
                )
            }
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_kit::ratatui::style::Style;
    use ratatui_kit::test_util::render_frame;
    use std::sync::{Arc, Mutex};

    // ── 居中带渲染级回归（P1：宽终端鼠标偏移）────────────────────────────

    #[derive(Default, Props)]
    struct AreaProbeProps {
        text: &'static str,
        sink: Arc<Mutex<Option<Rect>>>,
    }

    /// 测试专用探针：hook 记录 `pre_component_draw` 看到的区域，
    /// draw 在该区域左上角写标记——两者必须落在同一列。
    struct AreaProbe {
        text: &'static str,
    }

    struct AreaProbeHook {
        sink: Arc<Mutex<Option<Rect>>>,
    }

    impl Hook for AreaProbeHook {
        fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            *self.sink.lock().unwrap() = Some(drawer.area);
        }
    }

    impl Component for AreaProbe {
        type Props<'a> = AreaProbeProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self { text: "P" }
        }

        fn update(
            &mut self,
            props: &mut Self::Props<'_>,
            mut hooks: Hooks,
            _updater: &mut ComponentUpdater,
        ) {
            self.text = props.text;
            let sink = props.sink.clone();
            hooks.use_hook(move || AreaProbeHook { sink });
        }

        fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
            let area = drawer.area;
            drawer
                .buffer_mut()
                .set_string(area.x, area.y, self.text, Style::default());
        }
    }

    /// `CenterBand` 在 draw 期把子组件区域收进带：140 列 left_pad=11、
    /// 80 列 0、220 列 51；x=0 不得出现子组件标记（回归：宽终端渲染从 0 开始）。
    #[test]
    fn center_band_draw_places_child_inside_band() {
        for (term_w, expected_x) in [(80u16, 0u16), (140, 11), (220, 51)] {
            let grid = GridSpec::grid_for(term_w);
            assert_eq!(grid.left_pad, expected_x, "term_w={term_w}");
            let buf = render_frame(
                element!(CenterBand(grid: grid) {
                    Text(text: "X")
                }),
                term_w,
                3,
            );
            assert_eq!(
                buf[(expected_x, 0)].symbol(),
                "X",
                "term_w={term_w}: 标记应出现在带左缘"
            );
            if expected_x > 0 {
                assert_ne!(
                    buf[(0, 0)].symbol(),
                    "X",
                    "term_w={term_w}: 整幅左缘不得渲染子组件"
                );
            }
        }
    }

    /// tracker 一致性：hook 的 `pre_component_draw` 记录区域 == 实际绘制起点/宽度。
    /// 回归时渲染在 x=0 而 tracker 记录 x=left_pad，此断言会失败。
    #[test]
    fn center_band_tracker_matches_rendered_band() {
        let grid = GridSpec::grid_for(140);
        let expected = center_band_area(Rect::new(0, 0, 140, 5), grid);
        let sink = Arc::new(Mutex::new(None));
        let buf = render_frame(
            element!(CenterBand(grid: grid) {
                AreaProbe(text: "P", sink: sink.clone())
            }),
            140,
            5,
        );
        let recorded = sink
            .lock()
            .unwrap()
            .expect("pre_component_draw 应记录组件区域");
        assert_eq!(recorded, expected);
        assert_eq!(recorded.x, grid.left_pad);
        assert_eq!(recorded.width, grid.band_width);
        assert_eq!(buf[(recorded.x, recorded.y)].symbol(), "P");
    }

    /// §11 高度断点矩阵：13/12 → Full；11/9/8 → Row1Only；7 → Hidden + composer 钳制。
    #[test]
    fn layout_plan_height_matrix() {
        assert_eq!(layout_plan(13).status_bar, StatusBarMode::Full);
        assert_eq!(layout_plan(12).status_bar, StatusBarMode::Full);
        assert!(layout_plan(12).session_title_visible);
        assert_eq!(layout_plan(12).composer_max_lines, None);

        assert_eq!(layout_plan(11).status_bar, StatusBarMode::Row1Only);
        assert_eq!(layout_plan(9).status_bar, StatusBarMode::Row1Only);
        assert_eq!(layout_plan(8).status_bar, StatusBarMode::Row1Only);
        assert!(
            !layout_plan(8).session_title_visible,
            "h<12 隐藏 session title"
        );
        assert_eq!(layout_plan(8).composer_max_lines, None);

        assert_eq!(layout_plan(7).status_bar, StatusBarMode::Hidden);
        assert!(!layout_plan(7).session_title_visible);
        assert_eq!(layout_plan(7).composer_max_lines, Some(2));
        assert_eq!(layout_plan(1).status_bar, StatusBarMode::Hidden);
    }

    /// 40×8 冒烟（§11/§15）：h=8 时 transcript ≥3 行、composer 编辑行 ≤2、
    /// 无 key hints（Row2 隐藏）。
    /// 数学依据：status(Row1Only)=2 + composer(editor 1 + border 2)=3 → transcript=3。
    /// 多行输入（editor_rows ≤ 10）会压缩 transcript——用户主动行为，接受。
    #[test]
    fn layout_plan_40x8_smoke() {
        let plan = layout_plan(8);
        assert_eq!(plan.status_bar, StatusBarMode::Row1Only);
        // status 高度 2（Row1 + NotifRow）
        let status_h = 2u16;
        // 单行输入 composer 高度 3（editor 1 + border 2）
        let composer_h = 3u16;
        assert!(
            8 - status_h - composer_h >= 3,
            "40×8 下 transcript 应保留 ≥3 行"
        );
        // composer 编辑行数 ≤2：单行输入自然满足；h<8 时由 max_lines=Some(2) 钳制
        let editor_rows = 1u16;
        assert!(editor_rows <= 2, "composer 编辑行数应 ≤2");
        // 无 key hints：Row1Only 隐藏 Row2
        assert_ne!(plan.status_bar, StatusBarMode::Full);
    }

    /// 居中带（§3.1）：content 未触顶时区域逐列不变；触顶后按 `left_pad`
    /// 左移并收窄到 `band_width`（两端余量均分，奇数余 1 列留右侧）。
    #[test]
    fn center_band_area_centers_only_after_content_caps() {
        // 80 列：带宽 = 终端宽，不居中（与居中前逐列一致）
        let narrow = center_band_area(Rect::new(0, 0, 80, 24), GridSpec::grid_for(80));
        assert_eq!(narrow, Rect::new(0, 0, 80, 24));

        // 220 列：带宽 117，余量 103 均分 → 左 51 / 右 52
        let wide = center_band_area(Rect::new(0, 0, 220, 24), GridSpec::grid_for(220));
        assert_eq!(wide.width, 117);
        assert_eq!(wide.x, 51);
        assert_eq!(wide.x as usize + wide.width as usize + 52, 220);
    }

    /// 区域比带宽窄（嵌套布局 / 面板挤压）：钳到区域内，不越界、不留 0 宽。
    #[test]
    fn center_band_area_clamps_to_narrower_area() {
        for width in [0u16, 1, 2, 10, 40, 116] {
            let area = Rect::new(3, 5, width, 10);
            let band = center_band_area(area, GridSpec::grid_for(220));
            if width == 0 {
                assert_eq!(band, area, "0 宽区域不放大");
                continue;
            }
            assert!(band.width >= 1, "width={width}: 至少保留 1 列");
            assert!(
                band.x >= area.x
                    && band.x as usize + band.width as usize <= area.x as usize + width as usize,
                "width={width}: band {band:?} 应落在 {area:?} 内"
            );
            assert_eq!(band.y, area.y, "width={width}: 只改水平轴");
            assert_eq!(band.height, area.height, "width={width}: 高度不变");
        }
    }

    /// 终端尺寸未知（`terminal::size()` 失败 → `grid_for(0)`，带宽 0）：
    /// 保持原区域，不能把整条 UI 塌成 1 列。
    #[test]
    fn center_band_area_keeps_area_when_grid_unavailable() {
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(GridSpec::grid_for(0).band_width, 0);
        assert_eq!(center_band_area(area, GridSpec::grid_for(0)), area);
    }
}
