use super::*;

#[test]
fn right_align_col_keeps_wide_screen_value() {
    // 宽屏（≥100 列）：保持 40 列右对齐目标
    assert_eq!(right_align_col(100), 40);
    assert_eq!(right_align_col(120), 40);
}

#[test]
fn right_align_col_shrinks_on_narrow_screen() {
    // 窄屏：收缩到右列可容纳宽度（面板 - 45% 左列 - 分隔线 - 4 余量）
    assert_eq!(right_align_col(80), 39); // 80-36-1-4
    assert_eq!(right_align_col(60), 28); // 60-27-1-4
    assert_eq!(right_align_col(50), 23); // 50-22-1-4
    assert_eq!(right_align_col(40), 17); // 40-18-1-4
}

#[test]
fn right_align_col_never_underflows() {
    // 极窄：saturating 保证不为负
    assert_eq!(right_align_col(10), 1); // 10-4-1-4
    assert_eq!(right_align_col(0), 0); // 0*45/100=0 → 0-0-1 saturating → 0
}

#[test]
fn pad_fits_align_column() {
    // pad + key + value = align（值右边缘对齐），且 pad 不为负
    let align = right_align_col(60);
    let key_len = 9; // "    Model "
    let value_len = 12; // "gpt-5.6-luna"
    let pad = align.saturating_sub(key_len + value_len);
    assert_eq!(pad + key_len + value_len, align);
    assert!(pad < 32, "行宽应不超过右列可视宽度");
}

#[path = "model/commit_test.rs"]
mod commit;

// ── 面板级用例：共享视口、命中/绘制同源、滚动仲裁区域 ────────────────────────
//
// `State<T>` 无公开构造 API，句柄只能从真实挂载捕获（visitor 模式，同 memory /
// cron 面板测试）；档位切换经 `handle_model_event_with` 注入捕获闭包断言「点了
// 哪一行 → 切哪一档」，真实持久化由 `model/commit_test.rs` 覆盖。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{NOTIFICATION, Notification, PANEL_SCROLL_OWNER, PERI_CONFIG_HANDLE};
use crate::kit::panels::shared::list::{ListViewport, content_rows};
use ratatui_kit::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// 测试面板区域（渲染尺寸 = 组件区域）：内容区 16 行，四档卡片（header 2 +
/// 4×3 + footer 1 = 15）全部落在视口内。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 18,
};

/// 矮面板：内容区 10 行 → 可见 2 档（`(10-3)/3`）。
const SHORT_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 12,
};

/// 捕获到的档位切换下标。
type Switched = Rc<RefCell<Option<usize>>>;

fn switched() -> Switched {
    Rc::new(RefCell::new(None))
}

/// 确保配置句柄存在（OnceLock：其他用例可能已建立，此处只保证非空）。
fn ensure_config_handle() -> Arc<parking_lot::RwLock<crate::config::PeriConfig>> {
    let _ = PERI_CONFIG_HANDLE.set(Arc::new(parking_lot::RwLock::new(
        crate::config::PeriConfig::default(),
    )));
    Arc::clone(PERI_CONFIG_HANDLE.get().expect("配置句柄必须存在"))
}

/// 全局状态（激活档位 / 通知 / 滚动仲裁注册）的保存与恢复——断言失败也不污染
/// 其它测试。
struct ModelPanelGuard {
    handle: Arc<parking_lot::RwLock<crate::config::PeriConfig>>,
    active_alias: String,
    notification: Option<Notification>,
    scroll_owner: Option<crate::kit::panel_scroll::PanelScrollOwner>,
}

impl ModelPanelGuard {
    fn install() -> Self {
        let handle = ensure_config_handle();
        let active_alias = handle.read().config.active_alias.clone();
        let notification = std::mem::take(&mut *NOTIFICATION.state().write());
        let scroll_owner = std::mem::take(&mut *PANEL_SCROLL_OWNER.state().write());
        Self {
            handle,
            active_alias,
            notification,
            scroll_owner,
        }
    }

    fn set_active_alias(&self, alias: &str) {
        self.handle.write().config.active_alias = alias.to_string();
    }
}

impl Drop for ModelPanelGuard {
    fn drop(&mut self) {
        self.handle.write().config.active_alias = self.active_alias.clone();
        *NOTIFICATION.state().write() = self.notification.take();
        *PANEL_SCROLL_OWNER.state().write() = self.scroll_owner.take();
    }
}

fn left_down(row: u16, column: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn buffer_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// 缓冲中首次出现 `needle` 的终端行文本（即鼠标命中的绝对行号与整行内容）。
fn rendered_line_of(buf: &Buffer, needle: &str) -> (u16, String) {
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        if line.contains(needle) {
            return (y, line);
        }
    }
    panic!("渲染缓冲中找不到 {needle}");
}

// ── 挂载探针（`State<T>` 无公开构造 API，句柄只能从真实挂载中捕获）───────────

type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: RefCell<MountVisitor> = const { RefCell::new(None) };
}

pub(super) struct MountedPanel {
    pub(super) right_cursor: State<usize>,
    pub(super) right_focus: State<bool>,
    pub(super) render_version: State<u64>,
}

/// 卸载时清空 visitor：断言 panic 也不把句柄泄漏给同线程的后续用例。
struct VisitorGuard;

impl Drop for VisitorGuard {
    fn drop(&mut self) {
        MOUNT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

/// 每帧把当前句柄与帧序号交给 visitor（不消费）。
pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 挂载面板并渲染 `frames` 帧，返回末帧缓冲。
///
/// 句柄读取的视口来自 `AreaTracker`（上一帧区域），故断言渲染结果的用例至少
/// 渲染 2 帧才反映实测区域。
fn mount_panel_frames(
    area: Rect,
    frames: usize,
    mut visitor: impl FnMut(usize, MountedPanel) + 'static,
) -> Buffer {
    let _guard = VisitorGuard;
    let mut frame = 0usize;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(move |panel| {
            visitor(frame, panel);
            frame += 1;
        }));
    });
    ratatui_kit::test_util::render_frames(
        element!(super::ModelPanel()),
        area.width,
        area.height,
        frames,
    )
}

impl MountedPanel {
    /// 走真实事件入口：视口按 `area` + 当前激活档位推导，与组件渲染同源。
    fn event(&self, event: Event, area: Rect, switched: &Switched) -> EventResult {
        let active_idx = super::active_profile_idx(&super::current_active_alias());
        let viewport = ListViewport::for_content_rows(
            content_rows(Some(area), area.height),
            super::LIST_METRICS,
            PROFILE_KEYS.len(),
            active_idx,
        );
        super::handle_model_event_with(
            event,
            &super::ModelEventCtx {
                area: Some(area),
                right_cursor: self.right_cursor,
                right_focus: self.right_focus,
                render_version: self.render_version,
                viewport,
            },
            |idx| *switched.borrow_mut() = Some(idx),
        )
    }
}

/// 当前注册的面板滚动槽位区域（渲染期只有本面板注册）。
fn registered_scroll_areas() -> Vec<Rect> {
    PANEL_SCROLL_OWNER
        .state()
        .read()
        .as_ref()
        .map(|owner| {
            assert_eq!(owner.kind, PanelKind::Model);
            owner.slots.iter().map(|slot| slot.area).collect()
        })
        .unwrap_or_default()
}

// ── 视口随面板高度变化 ──────────────────────────────────────────────────────

/// 可见档位数由面板实测内容区行数推导，不再写死 4 档。
#[test]
#[serial]
fn rendered_profiles_follow_panel_height() {
    let guard = ModelPanelGuard::install();
    guard.set_active_alias("fable");

    let short = buffer_text(&mount_panel_frames(SHORT_AREA, 2, |_, _| {}));
    assert!(short.contains("fable ·"), "矮面板至少渲染第一档：{short}");
    assert!(short.contains("opus ·"), "内容区 10 行放得下 2 档：{short}");
    assert!(
        !short.contains("sonnet ·"),
        "内容区 10 行（2+2×3+1）只放得下 2 档，不应渲染第三档：{short}"
    );

    let tall = buffer_text(&mount_panel_frames(TEST_AREA, 2, |_, _| {}));
    assert!(
        tall.contains("haiku ·"),
        "面板变高后必须渲染全部四档，而不是停在矮面板的 2 档：{tall}"
    );
}

// ── 命中与绘制同源 ─────────────────────────────────────────────────────────

/// 点击渲染出的档位卡片 → 切换该档位，且下一帧把它渲染为当前档位。
#[test]
#[serial]
fn click_on_rendered_profile_card_switches_that_profile() {
    let guard = ModelPanelGuard::install();
    guard.set_active_alias("fable");
    let handle = Arc::clone(&guard.handle);

    let buf = mount_panel_frames(TEST_AREA, 2, |_, _| {});
    let (row, _) = rendered_line_of(&buf, "haiku ·");

    let switched = switched();
    let clicked = Rc::clone(&switched);
    let buf = mount_panel_frames(TEST_AREA, 3, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(left_down(row, 5), TEST_AREA, &clicked);
            assert_eq!(result, EventResult::Consumed);
            // 复现「切换落盘成功」后的内存效果：真实动作写的就是这一档
            let idx = clicked.borrow().expect("点击必须触发档位切换");
            handle.write().config.active_alias = PROFILE_KEYS[idx].to_string();
        }
    });

    assert_eq!(
        *switched.borrow(),
        Some(3),
        "点击必须切换被点的那一档（haiku）"
    );
    let text = buffer_text(&buf);
    let (_, haiku_line) = rendered_line_of(&buf, "haiku ·");
    assert!(
        haiku_line.trim_start().starts_with('●'),
        "被点中的档位必须在下一帧渲染为当前档位：\n{text}"
    );
    let (_, fable_line) = rendered_line_of(&buf, "fable ·");
    assert!(
        fable_line.trim_start().starts_with('○'),
        "未选中的档位必须渲染为普通项：\n{text}"
    );
}

/// 矮面板：窗口跟随当前档位滚动，命中取同一窗口——点击「渲染出的那一行」切换的
/// 是同一档（旧实现按写死的 4 档 + 偏移 0 命中，会切到窗口外的另一档）。
#[test]
#[serial]
fn short_panel_hit_matches_scrolled_window() {
    let guard = ModelPanelGuard::install();
    guard.set_active_alias("sonnet");

    let buf = mount_panel_frames(SHORT_AREA, 2, |_, _| {});
    let text = buffer_text(&buf);
    assert!(
        text.contains("sonnet ·") && text.contains("haiku ·"),
        "窗口必须滚到当前档位所在的末尾 2 档：{text}"
    );
    assert!(
        !text.contains("fable ·"),
        "内容区 10 行只放得下 2 档，窗口外的档位不得渲染：{text}"
    );
    let (last_row, _) = rendered_line_of(&buf, "haiku ·");
    let (active_row, _) = rendered_line_of(&buf, "sonnet ·");

    let switched = switched();
    mount_panel_frames(SHORT_AREA, 2, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(left_down(last_row, 5), SHORT_AREA, &switched);
            assert_eq!(result, EventResult::Consumed);
            assert_eq!(
                *switched.borrow(),
                Some(3),
                "点击渲染出的 haiku 卡片必须切换 haiku"
            );
            *switched.borrow_mut() = None;
            panel.event(left_down(active_row, 5), SHORT_AREA, &switched);
            assert_eq!(
                *switched.borrow(),
                Some(2),
                "点击渲染出的 sonnet 卡片必须切换 sonnet"
            );
        }
    });
}

/// 滚动条列（左栏最右 1 列）不参与列表命中。
#[test]
#[serial]
fn click_on_scrollbar_column_does_not_switch() {
    let _guard = ModelPanelGuard::install();
    let switched = switched();
    mount_panel_frames(SHORT_AREA, 2, {
        let switched = Rc::clone(&switched);
        move |_, panel| {
            // 左栏右边界列 = 其 ScrollView 的滚动条列（命中判定按 `< end` 排除）
            let column = super::left_column_end(SHORT_AREA);
            let result = panel.event(left_down(6, column), SHORT_AREA, &switched);
            assert_eq!(result, EventResult::Consumed, "区域内左键点击仍消费防穿透");
        }
    });
    assert!(switched.borrow().is_none(), "滚动条列点击不得触发档位切换");
}

/// 滚动仲裁注册区域与鼠标命中共用 `AreaTracker`：首帧未测量时 0 尺寸，
/// 之后为实测面板区域（此前用 `use_previous_size()`，与命中区域不同源）。
#[test]
#[serial]
fn scroll_areas_come_from_measured_panel_rect() {
    let _guard = ModelPanelGuard::install();

    mount_panel_frames(TEST_AREA, 2, |frame, _panel| {
        if frame == 1 {
            // 帧 0 组件尚未测量自身区域，注册 0 尺寸而不是猜测的尺寸
            assert_eq!(
                registered_scroll_areas(),
                vec![Rect::default(), Rect::default()]
            );
        }
    });

    let (left, right) = crate::kit::panel_scroll::split_vertical(TEST_AREA, 45);
    assert_eq!(
        registered_scroll_areas(),
        vec![left, right],
        "注册区域必须是 AreaTracker 的实测区域（与鼠标命中同源）"
    );
}

// ── hook 顺序（TUI-HOOK-001） ──────────────────────────────────────────────

/// 跨帧挂载：不同终端高度/焦点状态下 hook 数量与类型不变——ratatui-kit 的
/// `use_hook` 按调用下标取已注册 hook 并做类型校验，顺序偏移会直接 panic。
#[test]
#[serial]
fn hooks_stay_stable_across_frames() {
    let _guard = ModelPanelGuard::install();
    for area in [TEST_AREA, SHORT_AREA, Rect::new(0, 0, 80, 40)] {
        let buf = mount_panel_frames(area, 3, |frame, panel| {
            // 每帧切换焦点与字段光标：走不同的渲染分支
            *panel.right_focus.write() = frame % 2 == 1;
            *panel.right_cursor.write() = frame;
        });
        assert!(
            buffer_text(&buf).contains("Provider"),
            "每个尺寸/焦点组合都必须渲染出右栏字段"
        );
    }
}

/// TUI-HOOK-001（静态面）：组件体内所有 `hooks.use_*` 都在事件注册之前，且事件
/// 注册之前没有任何提前 `return`——hook 数量与顺序不可能随渲染路径变化。
#[test]
fn model_hooks_precede_event_registration_and_returns() {
    let src = include_str!("model.rs");
    let body = src
        .split("#[path = \"model_test.rs\"]")
        .next()
        .expect("组件模块体");
    let component = body.split("pub fn ModelPanel").nth(1).expect("组件函数体");
    let handler = component
        .find("hooks.use_event_handler_with_options")
        .expect("事件注册存在");

    assert!(
        component[..handler].contains("hooks.use_state")
            && component[..handler].contains("hooks.use_atom")
            && component[..handler].contains("hooks.use_hook")
            && component[..handler].contains("hooks.use_terminal_size"),
        "状态/atom/区域/终端尺寸 hook 都必须在事件注册之前无条件调用"
    );
    assert!(
        component.find("return ").is_none_or(|pos| pos > handler),
        "事件注册之前不得出现提前 return（hook 会被跳过）"
    );
    assert_eq!(
        component[handler..].matches("hooks.use_").count(),
        1,
        "事件注册之后不得再注册 hook"
    );
}
