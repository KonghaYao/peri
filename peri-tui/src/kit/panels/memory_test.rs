//! Tests for memory panel utilities.

use super::*;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, MEMORY_LIST, OPEN_PANELS};
use crate::kit::panels::shared::list::{ListViewport, content_rows};
use ratatui_kit::crossterm::event::{KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;

#[test]
fn test_format_size_bytes_below_kb() {
    // 0 走 fallthrough 路径（UNITS 全部 threshold 都不满足 0 >= t）
    assert_eq!(format_size(0), "0 B");
    // 1~9 走 B 分支：v = bytes/1.0 < 10 → "X.0 B"
    assert_eq!(format_size(1), "1.0 B");
    assert_eq!(format_size(9), "9.0 B");
    // 10~1023 走 B 分支：v >= 10 → "XX B"
    assert_eq!(format_size(10), "10 B");
    assert_eq!(format_size(512), "512 B");
    assert_eq!(format_size(1023), "1023 B");
}

#[test]
fn test_format_size_kb_threshold() {
    // 1024 = 1.0 KB（< 10，保留 1 位小数）
    assert_eq!(format_size(1024), "1.0 KB");
    // 1536 = 1.5 KB
    assert_eq!(format_size(1536), "1.5 KB");
    // 10240 = 10 KB（≥10，无小数）
    assert_eq!(format_size(10240), "10 KB");
    // 51200 = 50 KB
    assert_eq!(format_size(51200), "50 KB");
}

#[test]
fn test_format_size_mb_and_gb() {
    // 1 MB = 1048576
    assert_eq!(format_size(1048576), "1.0 MB");
    // 5 MB
    assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    // 1 GB
    assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
}

#[test]
fn test_format_size_u64_max_no_overflow() {
    // u64::MAX ~ 18.45 EB，但本函数最高只到 GB 单位——不应 panic 或 overflow
    let s = format_size(u64::MAX);
    assert!(
        s.ends_with(" GB"),
        "expected GB suffix for u64::MAX, got: {}",
        s
    );
}

#[test]
fn test_format_relative_time_just_now() {
    use chrono::Utc;
    let now = Utc::now();
    // 30s 前 → "just now"
    assert_eq!(format_relative_time(now), "just now");
    // 59s 前 → "just now"
    let almost_minute_ago = now - chrono::Duration::seconds(59);
    assert_eq!(format_relative_time(almost_minute_ago), "just now");
}

#[test]
fn test_format_relative_time_minutes() {
    use chrono::Utc;
    let now = Utc::now();
    let five_min_ago = now - chrono::Duration::minutes(5);
    assert_eq!(format_relative_time(five_min_ago), "5m ago");
    let fifty_nine_min_ago = now - chrono::Duration::minutes(59);
    assert_eq!(format_relative_time(fifty_nine_min_ago), "59m ago");
}

#[test]
fn test_format_relative_time_hours() {
    use chrono::Utc;
    let now = Utc::now();
    let two_hours_ago = now - chrono::Duration::hours(2);
    assert_eq!(format_relative_time(two_hours_ago), "2h ago");
    let twenty_three_hours_ago = now - chrono::Duration::hours(23);
    assert_eq!(format_relative_time(twenty_three_hours_ago), "23h ago");
}

#[test]
fn test_format_relative_time_days() {
    use chrono::Utc;
    let now = Utc::now();
    let five_days_ago = now - chrono::Duration::days(5);
    assert_eq!(format_relative_time(five_days_ago), "5d ago");
    let twenty_nine_days_ago = now - chrono::Duration::days(29);
    assert_eq!(format_relative_time(twenty_nine_days_ago), "29d ago");
}

#[test]
fn test_format_relative_time_over_30_days_falls_to_date() {
    use chrono::Utc;
    let now = Utc::now();
    let forty_days_ago = now - chrono::Duration::days(40);
    // 超过 30 天 → 显示 YYYY-MM-DD
    let result = format_relative_time(forty_days_ago);
    assert!(
        result.len() == 10 && result.chars().nth(4) == Some('-'),
        "expected date format YYYY-MM-DD, got: {}",
        result
    );
}

// ── 面板级用例：共享视口、命中/绘制同源、列表边界 ──────────────────────────
//
// `State<T>` 无公开构造 API，句柄只能从真实挂载捕获（visitor 模式，同 hooks/mcp
// 面板测试）；编辑器动作走 `intercept_editor_open` 接缝记录，不 spawn 真实进程。

/// 测试区域：宽 60、高 18（内容区 16 行 = header 3 + 13 项）。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 18,
};

fn memory_entry(index: usize) -> MemoryEntry {
    MemoryEntry {
        path: format!("mem{index:02}.md"),
        size_bytes: 1024,
        modified: None,
    }
}

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct MemoryListGuard(Vec<MemoryEntry>);

impl MemoryListGuard {
    fn install(items: Vec<MemoryEntry>) -> Self {
        let previous = std::mem::take(&mut *MEMORY_LIST.state().write());
        *MEMORY_LIST.state().write() = items;
        Self(previous)
    }
}

impl Drop for MemoryListGuard {
    fn drop(&mut self) {
        *MEMORY_LIST.state().write() = std::mem::take(&mut self.0);
    }
}

/// 面板栈：装成「Memory 面板已打开且在栈顶」，Esc/点击的关闭语义才有观察对象。
struct PanelStackGuard {
    open: Vec<PanelKind>,
    active: Option<PanelKind>,
}

impl PanelStackGuard {
    fn install() -> Self {
        let open = OPEN_PANELS.state().read().clone();
        let active = *ACTIVE_PANEL.state().read();
        *OPEN_PANELS.state().write() = vec![PanelKind::Memory];
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Memory);
        Self { open, active }
    }
}

impl Drop for PanelStackGuard {
    fn drop(&mut self) {
        *OPEN_PANELS.state().write() = std::mem::take(&mut self.open);
        *ACTIVE_PANEL.state().write() = self.active;
    }
}

thread_local! {
    /// 编辑器打开请求（测试接缝）——见 `intercept_editor_open`。
    static EDITOR_REQUESTS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 编辑器接缝：记录请求路径并返回 `true`（调用方据此跳过 spawn）。
///
/// 测试进程不得启动真实编辑器，`open_memory_in_editor` 在测试构建下总能拿到
/// `true`，因此路径解析、`$EDITOR` 解析与 spawn 错误分支不在单测覆盖范围内。
pub(super) fn intercept_editor_open(rel_path: &str) -> bool {
    EDITOR_REQUESTS.with(|slot| slot.borrow_mut().push(rel_path.to_string()));
    true
}

/// 取走并清空已记录的打开请求——断言「没有打开任何条目」前先取一次，
/// 避免同线程前一个用例的残留被算进来。
fn take_editor_requests() -> Vec<String> {
    EDITOR_REQUESTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// 挂载探针的访问器：`State<T>` 无公开构造 API，句柄只能从真实挂载中捕获。
type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: std::cell::RefCell<MountVisitor> =
        const { std::cell::RefCell::new(None) };
}

/// 卸载时清空 visitor：断言 panic 也不把句柄泄漏给同线程的后续用例。
struct VisitorGuard;

impl Drop for VisitorGuard {
    fn drop(&mut self) {
        MOUNT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

pub(super) struct MountedPanel {
    pub(super) selected: State<usize>,
}

impl MountedPanel {
    /// 走真实事件入口：视口按 `area` + 终端高度推导，与组件渲染同源。
    fn event(&self, event: Event, area: Rect, term_h: u16) -> EventResult {
        let item_count = MEMORY_LIST.state().read().len();
        let viewport = ListViewport::for_content_rows(
            content_rows(Some(area), term_h),
            super::LIST_METRICS,
            item_count,
            *self.selected.read(),
        );
        super::handle_memory_event(
            event,
            &super::MemoryEventCtx {
                area: Some(area),
                selected: self.selected,
                viewport,
            },
        )
    }
}

/// 每帧把当前句柄交给 visitor（不消费）：跨帧用例可在第 N 帧改状态、由后续帧收敛。
pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 挂载并渲染 `frames` 帧，返回末帧缓冲。
///
/// 句柄读取的视口来自 `AreaTracker`（上一帧区域），故首帧用 `panel_overlay` 的预测
/// 高度；断言渲染结果的用例至少渲染 2 帧才反映真实区域。
fn mount_panel_at(
    width: u16,
    height: u16,
    frames: usize,
    visitor: impl FnMut(MountedPanel) + 'static,
) -> Buffer {
    let _guard = VisitorGuard;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    ratatui_kit::test_util::render_frames(element!(super::MemoryPanel()), width, height, frames)
}

/// 单帧挂载（断言发生在渲染过程中，不需要缓冲）。
fn mount_panel(visitor: impl FnMut(MountedPanel) + 'static) {
    let _ = mount_panel_at(60, 18, 1, visitor);
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

/// 缓冲中首次出现 `needle` 的终端行（0 起）——即鼠标命中的绝对行号。
fn rendered_row_of(buf: &Buffer, needle: &str) -> u16 {
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        if line.contains(needle) {
            return y;
        }
    }
    panic!("渲染缓冲中找不到 {needle}");
}

/// 视口回归：可见项数由面板内容区行数推导，不再固定为写死的 13。
#[test]
#[serial]
fn visible_items_follow_panel_height() {
    let _list = MemoryListGuard::install((0..20).map(memory_entry).collect());

    let short = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::MemoryPanel()),
        60,
        10,
        2,
    ));
    assert!(short.contains("mem00"), "矮面板至少渲染第一项");
    assert!(
        !short.contains("mem05"),
        "内容区 8 行只放得下 5 项（3 + 5），不应渲染第六项"
    );

    let tall = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::MemoryPanel()),
        60,
        40,
        2,
    ));
    assert!(
        tall.contains("mem19"),
        "面板变高后必须渲染更多项，而不是停在写死的 13 项"
    );
}

/// 矮面板的视口跟随按自身高度计算：选中末尾项时窗口落在末尾 5 项，
/// 而不是按面板高度 18 写死的 13 行从中间项起。
#[test]
#[serial]
fn short_panel_scroll_follow_uses_its_own_viewport() {
    let _list = MemoryListGuard::install((0..20).map(memory_entry).collect());

    let mut frame = 0usize;
    let buf = mount_panel_at(60, 10, 2, move |panel| {
        if frame == 0 {
            *panel.selected.write() = 19;
        }
        frame += 1;
    });
    let text = buffer_text(&buf);
    assert!(
        text.contains("> mem19.md"),
        "选中末尾项必须渲染在窗口内：\n{text}"
    );
    assert!(
        !text.contains("mem00"),
        "内容区 8 行只放得下 5 项，窗口应从 mem15 起（不得按 13 行从 mem07 起）：\n{text}"
    );
}

/// 点击落在某行渲染出的位置 = 选中该行并打开它的 path（命中布局与绘制同源）。
#[test]
#[serial]
fn click_on_rendered_row_selects_it_and_opens_that_path() {
    let _list = MemoryListGuard::install((0..4).map(memory_entry).collect());
    let _panels = PanelStackGuard::install();
    let _ = take_editor_requests();

    let buf = ratatui_kit::test_util::render_frames(element!(super::MemoryPanel()), 60, 18, 2);
    let row = rendered_row_of(&buf, "mem01.md");
    assert_eq!(row, 5, "第二项渲染在内容区 header 之后的第二行");

    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel(move |panel| {
        let result = panel.event(left_down(row, 5), TEST_AREA, 18);
        assert_eq!(*panel.selected.read(), 1, "点击必须选中被点的那一项");
        tx.send(result).expect("测试结果通道仍打开");
    });

    assert_eq!(rx.try_recv().ok(), Some(EventResult::Consumed));
    assert_eq!(
        take_editor_requests(),
        vec!["mem01.md".to_string()],
        "click as enter：打开被点击行的 path"
    );
}

/// Enter 打开**当前选中**条目的 path——边移动边取，不用进入面板时的快照。
#[test]
#[serial]
fn enter_opens_the_selected_entry_path() {
    let _list = MemoryListGuard::install((0..4).map(memory_entry).collect());
    let _panels = PanelStackGuard::install();
    let _ = take_editor_requests();

    mount_panel(|panel| {
        panel.event(key(KeyCode::Down), TEST_AREA, 18);
        assert_eq!(*panel.selected.read(), 1);
        assert_eq!(
            panel.event(key(KeyCode::Enter), TEST_AREA, 18),
            EventResult::Consumed
        );
        assert_eq!(
            take_editor_requests(),
            vec!["mem01.md".to_string()],
            "Enter 打开被选中行的 path"
        );
    });
}

#[test]
#[serial]
fn click_on_scrollbar_column_does_not_select() {
    let _list = MemoryListGuard::install((0..4).map(memory_entry).collect());
    let _panels = PanelStackGuard::install();
    let _ = take_editor_requests();

    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel(move |panel| {
        // 最右列是滚动条列，不触发列表动作
        let result = panel.event(left_down(5, 59), TEST_AREA, 18);
        assert_eq!(*panel.selected.read(), 0);
        tx.send(result).expect("测试结果通道仍打开");
    });

    assert_eq!(rx.try_recv().ok(), Some(EventResult::Consumed));
    assert!(
        take_editor_requests().is_empty(),
        "滚动条列点击不得打开任何条目"
    );
}

/// ↑↓ 在列表边界停住（含空列表），且移动不触发编辑器。
#[test]
#[serial]
fn arrows_stay_within_list_bounds() {
    let _list = MemoryListGuard::install((0..3).map(memory_entry).collect());
    let _panels = PanelStackGuard::install();

    mount_panel(|panel| {
        assert_eq!(
            panel.event(key(KeyCode::Up), TEST_AREA, 18),
            EventResult::Consumed
        );
        assert_eq!(*panel.selected.read(), 0, "首项再 ↑ 不越界");

        for _ in 0..4 {
            panel.event(key(KeyCode::Down), TEST_AREA, 18);
        }
        assert_eq!(*panel.selected.read(), 2, "末项再 ↓ 停住");
        assert!(take_editor_requests().is_empty(), "移动不打开编辑器");
    });

    // 空列表：↓ 不给选择留下越界值
    {
        let _empty = MemoryListGuard::install(Vec::new());
        mount_panel(|panel| {
            panel.event(key(KeyCode::Down), TEST_AREA, 18);
            assert_eq!(*panel.selected.read(), 0);
        });
    }
}

/// Esc 关闭面板；Ctrl-C 被面板整帧吞掉（不落到全局退出路径）。
#[test]
#[serial]
fn esc_closes_panel_and_ctrl_c_is_swallowed() {
    let _list = MemoryListGuard::install((0..3).map(memory_entry).collect());
    let _panels = PanelStackGuard::install();

    mount_panel(|panel| {
        assert_eq!(
            panel.event(key(KeyCode::Esc), TEST_AREA, 18),
            EventResult::Consumed
        );
        assert_eq!(
            panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                TEST_AREA,
                18
            ),
            EventResult::Consumed,
            "Ctrl-C 必须被面板吞掉"
        );
    });

    assert_eq!(*ACTIVE_PANEL.state().read(), None, "Esc 关闭面板");
    assert!(
        OPEN_PANELS.state().read().is_empty(),
        "Esc 把 Memory 从面板栈弹出"
    );
}
