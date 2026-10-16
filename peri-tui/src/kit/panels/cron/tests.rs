//! Cron 面板测试：动态视口、click as enter、确认删除的稳定身份与完成门。
//!
//! 动作出口经 `handle_cron_event_with` 注入捕获闭包，断言「对哪个 id 做什么」；
//! 真实 RPC 的会话守卫与线上参数由 `cron/actions_test.rs` 覆盖。

use std::cell::RefCell;
use std::rc::Rc;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{CRON_ACTION_ERROR, CRON_JOBS, CronJobSummary, PANEL_SCROLL_OWNER};
use crate::kit::panels::shared::list::{ListViewport, content_rows};
use ratatui_kit::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;

/// 挂载渲染尺寸 = 测试用面板区域：内容区 16 行（`content_rows` 扣掉上下边框）。
/// header 3 行后每项 3 行 → 第 i 项占内容行 `3 + 3i .. 3 + 3i + 2`，
/// 即绝对行 `4 + 3i .. 6 + 3i`（`area.y` 是顶部边框行）。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 18,
};

/// 捕获到的动作序列（本地决策；不触发真实 RPC）。
type Captured = Rc<RefCell<Vec<(&'static str, String)>>>;

fn captured() -> Captured {
    Rc::new(RefCell::new(Vec::new()))
}

fn job(index: usize) -> CronJobSummary {
    CronJobSummary {
        id: format!("job{index}"),
        expression: format!("expr{index:02}"),
        prompt: format!("prompt{index:02}"),
        enabled: true,
        next_fire: None,
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
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

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct PanelAtomsGuard {
    jobs: Vec<CronJobSummary>,
    action_error: Option<String>,
    scroll_owner: Option<crate::kit::panel_scroll::PanelScrollOwner>,
}

impl PanelAtomsGuard {
    fn install(jobs: Vec<CronJobSummary>) -> Self {
        let previous = Self {
            jobs: CRON_JOBS.state().read().clone(),
            action_error: CRON_ACTION_ERROR.state().read().clone(),
            scroll_owner: PANEL_SCROLL_OWNER.state().read().clone(),
        };
        *CRON_JOBS.state().write() = jobs;
        *CRON_ACTION_ERROR.state().write() = None;
        previous
    }
}

impl Drop for PanelAtomsGuard {
    fn drop(&mut self) {
        *CRON_JOBS.state().write() = std::mem::take(&mut self.jobs);
        *CRON_ACTION_ERROR.state().write() = self.action_error.take();
        *PANEL_SCROLL_OWNER.state().write() = self.scroll_owner.take();
    }
}

// ── 挂载探针（`State<T>` 无公开构造 API，句柄只能从真实挂载中捕获）───────────

type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: RefCell<MountVisitor> = const { RefCell::new(None) };
}

pub(super) struct MountedPanel {
    pub(super) selected: State<usize>,
    pub(super) confirm_delete: State<Option<String>>,
    pub(super) action_gates: State<super::actions::ActionGates>,
}

/// 卸载时清空 visitor：断言 panic 也不把句柄泄漏给同线程的后续用例。
struct VisitorGuard;

impl Drop for VisitorGuard {
    fn drop(&mut self) {
        MOUNT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

/// 每帧把当前句柄交给 visitor（不消费）：跨帧用例可在第 N 帧断言前 N-1 帧的效果。
pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 挂载面板并渲染 `frames` 帧；visitor 每帧收到 (帧序号, 当前句柄)，返回末帧缓冲。
pub(super) fn mount_panel_frames(
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
        element!(super::CronPanel()),
        TEST_AREA.width,
        TEST_AREA.height,
        frames,
    )
}

/// 单帧挂载（不需跨帧断言的用例）。
fn mount_panel(mut visitor: impl FnMut(MountedPanel) + 'static) -> Buffer {
    mount_panel_frames(1, move |_, panel| visitor(panel))
}

struct ResetGuard(u64);

impl Drop for ResetGuard {
    fn drop(&mut self) {
        crate::kit::atoms::BRIDGE_RESET_COUNTER.set(self.0);
    }
}

#[test]
#[serial]
fn reset_effect_cancels_pending_ticket_on_next_frame() {
    let _atoms = PanelAtomsGuard::install(vec![job(0)]);
    let _reset = ResetGuard(crate::kit::atoms::BRIDGE_RESET_COUNTER.get());
    let mut ticket = None;
    mount_panel_frames(3, move |frame, panel| match frame {
        1 => {
            ticket = Some(
                panel
                    .action_gates
                    .write()
                    .entry("job0".into())
                    .or_default()
                    .begin(super::actions::current_scope()),
            );
            crate::kit::atoms::BRIDGE_RESET_COUNTER
                .set(crate::kit::atoms::BRIDGE_RESET_COUNTER.get() + 1);
        }
        2 => {
            assert!(ticket.as_ref().unwrap().is_cancelled());
            assert!(panel.action_gates.read().is_empty());
        }
        _ => {}
    });
}

impl MountedPanel {
    /// 走真实事件入口（动作出口注入捕获闭包）：视口按 `TEST_AREA` 推导，与组件渲染同源。
    fn event(&self, event: Event, captured: &Captured) -> EventResult {
        self.event_in(event, TEST_AREA, captured)
    }

    /// 指定面板区域的同一入口——用于断言矮面板的命中窗口。
    fn event_in(&self, event: Event, area: Rect, captured: &Captured) -> EventResult {
        let jobs = CRON_JOBS.state().read().clone();
        let viewport = ListViewport::for_content_rows(
            content_rows(Some(area), area.height),
            super::list_metrics(jobs.is_empty()),
            jobs.len(),
            *self.selected.read(),
        );
        super::handle_cron_event_with(
            event,
            &super::CronEventCtx {
                area: Some(area),
                selected: self.selected,
                confirm_delete: self.confirm_delete,
                action_gates: self.action_gates,
                action_scope: super::actions::current_scope(),
                viewport,
                jobs: &jobs,
            },
            |method, id| captured.borrow_mut().push((method, id.to_string())),
        )
    }
}

/// 当前注册的面板滚动槽位区域（渲染期只有本面板注册）。
fn registered_scroll_area() -> Option<Rect> {
    PANEL_SCROLL_OWNER.state().read().as_ref().map(|owner| {
        assert_eq!(owner.kind, PanelKind::Cron);
        owner.slots.first().expect("Cron 面板注册单槽位").area
    })
}

// ── 视口随面板高度变化 ──────────────────────────────────────────────────────

/// 可见项数由面板实测内容区行数决定，不再固定为 4。
#[test]
#[serial]
fn visible_items_follow_panel_height() {
    let _guard = PanelAtomsGuard::install((0..12).map(job).collect());

    // 60×8：内容区 6 行 = header 3 + 1 项 ×3 行
    let short = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::CronPanel()),
        60,
        8,
        2,
    ));
    assert!(short.contains("expr00"), "矮面板至少渲染第一项：{short}");
    assert!(
        !short.contains("expr01"),
        "内容区 6 行（3+3）只放得下 1 项，不应渲染第二项：{short}"
    );

    // 60×40：内容区 38 行 = header 3 + 11 项 ×3 行
    let tall = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::CronPanel()),
        60,
        40,
        2,
    ));
    assert!(
        tall.contains("expr10"),
        "面板变高后必须渲染更多项，而不是停在写死的 4 项：{tall}"
    );
    assert!(
        !tall.contains("expr11"),
        "可见项数由内容区行数推导（11 项），第 12 项应被裁剪：{tall}"
    );
}

/// 滚动仲裁注册区域与鼠标命中共用 `AreaTracker`：首帧未测量时 0 尺寸，
/// 之后为实测面板区域。
#[test]
#[serial]
fn scroll_area_comes_from_measured_panel_rect() {
    let _guard = PanelAtomsGuard::install((0..4).map(job).collect());

    mount_panel_frames(2, |frame, _panel| {
        if frame == 1 {
            // 第 1 帧（帧 0）组件尚未测量自身区域，注册 0 尺寸而不是猜测的尺寸
            assert_eq!(registered_scroll_area(), Some(Rect::default()));
        }
    });

    assert_eq!(
        registered_scroll_area(),
        Some(TEST_AREA),
        "注册区域必须是 AreaTracker 的实测区域（与鼠标命中同源）"
    );
}

/// 绘制窗口与鼠标命中同源：面板变矮后，可见窗口之外的行不可点
/// （可见项写死时命中会比绘制多出一项）。
#[test]
#[serial]
fn hit_layout_follows_rendered_window_on_short_panel() {
    let _guard = PanelAtomsGuard::install((0..4).map(job).collect());
    let captured = captured();
    // 60×12：内容区 10 行 = header 3 + 可见 2 项 ×3 行
    let short = Rect {
        height: 12,
        ..TEST_AREA
    };

    mount_panel({
        let captured = Rc::clone(&captured);
        move |panel| {
            // 第 2 项（内容行 6..8 → 绝对行 7..9）在可见窗口内，可点
            panel.event_in(left_down(9, 5), short, &captured);
            assert_eq!(*panel.selected.read(), 1);
            // 内容行 9 起是窗口外的第 3 项（footer 起点）：不可点
            let result = panel.event_in(left_down(10, 5), short, &captured);
            assert_eq!(result, EventResult::Consumed, "区域内未命中行仍消费左键");
            assert_eq!(*panel.selected.read(), 1, "可见窗口外的行不改变选中项");
        }
    });

    assert_eq!(
        captured.borrow().as_slice(),
        [(super::CRON_TOGGLE_METHOD, "job1".to_string())],
        "只有可见窗口内的行能触发动作"
    );
}

// ── 鼠标与键盘同语义（click as enter）──────────────────────────────────────

#[test]
#[serial]
fn click_on_row_selects_and_toggles_that_row() {
    let _guard = PanelAtomsGuard::install((0..4).map(job).collect());
    let captured = captured();

    mount_panel({
        let captured = Rc::clone(&captured);
        move |panel| {
            // 点击第二项（内容行 6..8 → 绝对行 7..9）
            let result = panel.event(left_down(8, 5), &captured);
            assert_eq!(*panel.selected.read(), 1, "点击必须选中被点的那一项");
            assert_eq!(result, EventResult::Consumed);
        }
    });

    assert_eq!(
        captured.borrow().as_slice(),
        [(super::CRON_TOGGLE_METHOD, "job1".to_string())],
        "点击 = 该行 Enter 动作：toggle 目标必须是被点行"
    );
}

#[test]
#[serial]
fn click_on_scrollbar_column_does_not_toggle() {
    let _guard = PanelAtomsGuard::install((0..4).map(job).collect());
    let captured = captured();

    mount_panel({
        let captured = Rc::clone(&captured);
        move |panel| {
            // 最右列是滚动条列，不触发列表动作
            let result = panel.event(left_down(8, TEST_AREA.width - 1), &captured);
            assert_eq!(*panel.selected.read(), 0, "滚动条列不改变选中项");
            assert_eq!(result, EventResult::Consumed);
        }
    });

    assert!(captured.borrow().is_empty(), "滚动条列点击不得触发动作");
}

// ── 确认删除的稳定身份与完成门 ──────────────────────────────────────────────

/// 打开确认后列表重排：删除命中的仍是确认时捕获的 id（下标已被刷新改变）。
#[test]
#[serial]
fn confirm_delete_uses_captured_id_after_reorder() {
    let _guard = PanelAtomsGuard::install(vec![job(0), job(1), job(2)]);
    let captured = captured();

    let buf = mount_panel_frames(2, {
        let captured = Rc::clone(&captured);
        move |frame, panel| match frame {
            0 => {
                *panel.selected.write() = 1; // 选中 job1
                panel.event(key(KeyCode::Char('d')), &captured);
                assert_eq!(
                    panel.confirm_delete.read().as_deref(),
                    Some("job1"),
                    "确认模式固定待删任务的 id"
                );
                // 列表刷新：顺序变化，job1 从下标 1 移到下标 2（下标 1 现在是 job0）
                *CRON_JOBS.state().write() = vec![job(2), job(0), job(1)];
            }
            _ => {
                assert_eq!(
                    panel.confirm_delete.read().as_deref(),
                    Some("job1"),
                    "列表刷新不改变待执行目标"
                );
                panel.event(key(KeyCode::Enter), &captured);
                assert!(
                    panel.confirm_delete.read().is_none(),
                    "确认执行后必须退出确认模式"
                );
            }
        }
    });

    assert_eq!(
        captured.borrow().as_slice(),
        [(super::CRON_REMOVE_METHOD, "job1".to_string())],
        "删除必须落在确认时捕获的 id 上，而不是重排后的同一个下标"
    );

    let text = buffer_text(&buf);
    let pos = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} 必须渲染"))
    };
    assert!(
        pos("expr02") < pos("expr00") && pos("expr00") < pos("expr01"),
        "确认期间列表按刷新后的顺序渲染：{text}"
    );
}

/// 确认期间目标从列表消失：忽略本次确认，不得改删别的任务。
#[test]
#[serial]
fn confirm_target_gone_skips_delete() {
    let _guard = PanelAtomsGuard::install(vec![job(0), job(1)]);
    let captured = captured();

    mount_panel({
        let captured = Rc::clone(&captured);
        move |panel| {
            *panel.selected.write() = 0;
            panel.event(key(KeyCode::Char('d')), &captured);
            assert_eq!(panel.confirm_delete.read().as_deref(), Some("job0"));

            // job0 从列表消失（别处删除 / 刷新），下标 0 现在是 job1
            *CRON_JOBS.state().write() = vec![job(1)];
            let result = panel.event(key(KeyCode::Enter), &captured);
            assert_eq!(result, EventResult::Consumed);
            assert!(
                panel.confirm_delete.read().is_none(),
                "忽略本次确认后退出确认模式"
            );
        }
    });

    assert!(
        captured.borrow().is_empty(),
        "目标消失时不得发出任何删除：{:?}",
        captured.borrow()
    );
}

/// 完成门：确认态一次性清除，重复 Enter 不会连发删除。
#[test]
#[serial]
fn repeated_enter_after_confirm_does_not_delete_again() {
    let _guard = PanelAtomsGuard::install(vec![job(0)]);
    let captured = captured();

    mount_panel({
        let captured = Rc::clone(&captured);
        move |panel| {
            panel.event(key(KeyCode::Char('d')), &captured);
            panel.event(key(KeyCode::Enter), &captured);
            assert!(panel.confirm_delete.read().is_none());
            // 第二次 Enter 已不在确认模式：只 toggle，不再 remove
            panel.event(key(KeyCode::Enter), &captured);
        }
    });

    assert_eq!(
        captured.borrow().as_slice(),
        [
            (super::CRON_REMOVE_METHOD, "job0".to_string()),
            (super::CRON_TOGGLE_METHOD, "job0".to_string()),
        ],
        "同一次确认只允许一次删除"
    );
}

/// 确认模式下的点击不触发 toggle（防止误删时顺手切换任务）。
#[test]
#[serial]
fn click_while_confirming_does_not_toggle() {
    let _guard = PanelAtomsGuard::install(vec![job(0), job(1)]);
    let captured = captured();

    let buf = mount_panel({
        let captured = Rc::clone(&captured);
        move |panel| {
            panel.event(key(KeyCode::Char('d')), &captured);
            assert_eq!(panel.confirm_delete.read().as_deref(), Some("job0"));

            // 点击第二项：确认模式下只消费事件
            let result = panel.event(left_down(8, 5), &captured);
            assert_eq!(result, EventResult::Consumed);
            assert_eq!(*panel.selected.read(), 0, "确认模式下点击不改变选中项");
            assert_eq!(
                panel.confirm_delete.read().as_deref(),
                Some("job0"),
                "点击不取消确认模式"
            );
        }
    });

    assert!(captured.borrow().is_empty(), "确认期间点击不得触发动作");
    let text = buffer_text(&buf);
    assert!(
        text.contains(crate::i18n::tr("panel-cron-confirm-hint").trim()),
        "待删目标仍固定时确认提示必须可见：{text}"
    );
}
