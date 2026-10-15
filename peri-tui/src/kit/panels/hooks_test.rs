//! HooksPanel 底部提示与真实动作的一致性回归。

use crate::kit::atoms::{ACTIVE_PANEL, HOOK_LIST, HookSummary, OPEN_PANELS};
use crate::kit::panels::shared::list::{ListMetrics, ListViewport, content_rows};
use ratatui_kit::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;

/// 面板行度量：header 3 行 + 每项 3 行 + footer 2 行（与组件内常量同源）。
const METRICS: ListMetrics = ListMetrics::new(3, 3, 2);

fn hook(index: usize) -> HookSummary {
    HookSummary {
        event: format!("hooke{index:02}"),
        plugin_name: "plug".to_string(),
        command: format!("cmd{index}"),
        matcher: None,
    }
}

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct HookListGuard(Vec<HookSummary>);

impl HookListGuard {
    fn install(items: Vec<HookSummary>) -> Self {
        let previous = std::mem::take(&mut *HOOK_LIST.state().write());
        *HOOK_LIST.state().write() = items;
        Self(previous)
    }
}

impl Drop for HookListGuard {
    fn drop(&mut self) {
        *HOOK_LIST.state().write() = std::mem::take(&mut self.0);
    }
}

struct PanelStackGuard {
    open: Vec<crate::app::panel_types::PanelKind>,
    active: Option<crate::app::panel_types::PanelKind>,
}

impl PanelStackGuard {
    fn install() -> Self {
        let open = OPEN_PANELS.state().read().clone();
        let active = *ACTIVE_PANEL.state().read();
        *OPEN_PANELS.state().write() = Vec::new();
        *ACTIVE_PANEL.state().write() = None;
        Self { open, active }
    }
}

impl Drop for PanelStackGuard {
    fn drop(&mut self) {
        *OPEN_PANELS.state().write() = std::mem::take(&mut self.open);
        *ACTIVE_PANEL.state().write() = self.active;
    }
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

fn left_down(row: u16, column: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// 视口回归：可见项数由面板内容区行数决定，不再固定为 3。
#[test]
#[serial]
fn visible_items_follow_panel_height() {
    let _guard = HookListGuard::install((0..12).map(hook).collect());

    let short = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::HooksPanel()),
        60,
        10,
        2,
    ));
    assert!(short.contains("hooke00"), "矮面板至少渲染第一项");
    assert!(
        !short.contains("hooke01"),
        "内容区 8 行只放得下 1 项（3+3+2），不应渲染第二项"
    );

    let tall = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::HooksPanel()),
        60,
        40,
        2,
    ));
    assert!(
        tall.contains("hooke09"),
        "面板变高后必须渲染更多项，而不是停在写死的 3 项"
    );
}

// ── 鼠标与键盘同语义（click as enter）─────────────────────────────────────

thread_local! {
    static MOUNT_VISITOR: std::cell::RefCell<Option<Box<dyn FnOnce(MountedPanel)>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) struct MountedPanel {
    pub(super) selected: State<usize>,
}

impl MountedPanel {
    /// 走真实事件入口：视口按 `area` 推导，与组件渲染同源。
    fn event(&self, event: Event, area: Rect, term_h: u16) -> EventResult {
        let item_count = HOOK_LIST.state().read().len();
        let viewport = ListViewport::for_content_rows(
            content_rows(Some(area), term_h),
            METRICS,
            item_count,
            *self.selected.read(),
        );
        super::handle_hooks_event(event, Some(area), self.selected, viewport)
    }
}

pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().take() {
            visitor(panel);
        }
    });
}

fn mount_panel(visitor: impl FnOnce(MountedPanel) + 'static) {
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "visitor 必须在挂载中被消费");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    let _ = ratatui_kit::test_util::render_frame(element!(super::HooksPanel()), 60, 18);
}

#[test]
#[serial]
fn click_on_row_selects_it_and_closes_panel() {
    let _hooks = HookListGuard::install((0..4).map(hook).collect());
    let _panels = PanelStackGuard::install();

    // 面板 60×18：内容区 16 行 → header 3 + 3 项 + footer 2 可用；第二项占内容行 6..8
    let area = Rect::new(0, 0, 60, 18);
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel(move |panel| {
        let result = panel.event(left_down(8, 5), area, 18);
        assert_eq!(*panel.selected.read(), 1, "点击必须选中被点的那一项");
        tx.send(result).expect("测试结果通道仍打开");
    });

    assert_eq!(rx.try_recv().ok(), Some(EventResult::Consumed));
    assert_eq!(
        *ACTIVE_PANEL.state().read(),
        None,
        "点击与 Enter 同语义：关闭面板"
    );
}

#[test]
#[serial]
fn click_on_scrollbar_column_does_not_select() {
    let _hooks = HookListGuard::install((0..4).map(hook).collect());
    let _panels = PanelStackGuard::install();

    let area = Rect::new(0, 0, 60, 18);
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel(move |panel| {
        // 最右列是滚动条列，不触发列表动作
        let result = panel.event(left_down(8, 59), area, 18);
        assert_eq!(*panel.selected.read(), 0);
        tx.send(result).expect("测试结果通道仍打开");
    });

    assert_eq!(rx.try_recv().ok(), Some(EventResult::Consumed));
}

/// 只读 Info 面板（333c51d4）：Enter 与 Esc 一样关闭面板，提示必须描述真实动作。
#[test]
fn test_hooks_nav_hint_describes_close_not_open() {
    let src = include_str!("hooks.rs");
    // 组件内另有 `#[cfg(test)]` 挂载探针，模块体按测试模块声明的路径属性切分
    let main_src = src
        .split("#[path = \"hooks_test.rs\"]")
        .next()
        .expect("hooks module body");
    assert!(
        main_src.contains(r#"i18n::tr("panel-hooks-nav-hint")"#),
        "Hooks 面板底部提示须使用 Hooks 专属 key"
    );
    // 动作侧：Enter 分支（到下一个 key 分支为止）调用 close_panel
    let enter_arm = main_src
        .split("KeyCode::Enter")
        .nth(1)
        .and_then(|rest| rest.split("KeyCode::").next())
        .expect("hooks Enter 分支存在");
    assert!(
        enter_arm.contains("close_panel()"),
        "Hooks 面板 Enter 的真实动作是关闭面板；动作语义变化须同步提示与该用例"
    );
    for lang in ["en", "zh-CN"] {
        let hint = crate::i18n::LcRegistry::new(Some(lang)).tr("panel-hooks-nav-hint");
        assert!(
            !hint.contains("Enter::open"),
            "{lang} 提示不得把关闭伪装成打开：{hint}"
        );
        assert!(
            hint.contains("Enter::close") || hint.contains("Enter::关闭"),
            "{lang} 提示须标明 Enter 关闭：{hint}"
        );
    }
}
