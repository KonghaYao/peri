//! TasksPanel 回归：选中收敛、绘制与命中同源、取消身份解析与鼠标语义。
//!
//! [Why] 事件入口依赖 `State<T>` 句柄（无公开构造 API），故经挂载探针
//! `visit_mounted_panel` 从真实挂载中捕获句柄后投喂事件；取消请求与会话身份由测试接缝
//! 注入——单测进程里 `ACP_CLIENT_HANDLE` 由其它用例的 `OnceLock` 决定，构造不出可控的
//! 会话环境。

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Receiver;

use super::*;
use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
use ratatui_kit::crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
use ratatui_kit::ratatui::buffer::Buffer;
use serial_test::serial;

mod operations;
mod source;
mod viewport;

/// 测试面板尺寸——与渲染缓冲一致，绘制行号与命中行号同源。
const TEST_WIDTH: u16 = 60;
const TEST_HEIGHT: u16 = 18;
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: TEST_WIDTH,
    height: TEST_HEIGHT,
};
/// 内容区行数（不含上下边框）——与组件内 `content_rows(area, term_h)` 同源。
const TEST_CONTENT_ROWS: u16 = TEST_HEIGHT - 2;

// ── 取消请求与会话身份的测试接缝 ────────────────────────────────────────

thread_local! {
    /// 已记录的取消请求：(会话, 目标)。
    static CANCEL_REQUESTS: RefCell<Vec<(String, CancelTarget)>> = const { RefCell::new(Vec::new()) };
    /// 接缝是否接管取消请求。
    static CANCEL_ARMED: Cell<bool> = const { Cell::new(false) };
    static CANCEL_TICKETS: RefCell<Vec<(OperationTicket, State<CancelGates>, CancelTarget)>> = const { RefCell::new(Vec::new()) };
    /// 强制的会话身份：`Some(None)` = 无活动会话。
    static FORCED_SESSION: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// 测试接缝：记录取消请求并阻止真实发送；未接管时返回 false（走真实发送分支）。
pub(super) fn intercept_cancel_request(session_id: &str, target: &CancelTarget) -> bool {
    if !CANCEL_ARMED.with(Cell::get) {
        return false;
    }
    CANCEL_REQUESTS.with(|list| {
        list.borrow_mut()
            .push((session_id.to_string(), target.clone()))
    });
    true
}

pub(super) fn capture_cancel_ticket(
    ticket: OperationTicket,
    gates: State<CancelGates>,
    target: CancelTarget,
) {
    CANCEL_TICKETS.with(|tickets| tickets.borrow_mut().push((ticket, gates, target)));
}

/// 测试接缝：强制取消动作看到的会话身份（生产路径按 ACP client 解析）。
pub(super) fn forced_session() -> Option<Option<String>> {
    FORCED_SESSION.with(|slot| slot.borrow().clone())
}

/// 取消请求记录器：安装期间请求被记录而不发送。
struct CancelGuard;

impl CancelGuard {
    fn install() -> Self {
        CANCEL_REQUESTS.with(|list| list.borrow_mut().clear());
        CANCEL_TICKETS.with(|tickets| tickets.borrow_mut().clear());
        CANCEL_ARMED.with(|armed| armed.set(true));
        Self
    }

    fn requests() -> Vec<(String, CancelTarget)> {
        CANCEL_REQUESTS.with(|list| list.borrow().clone())
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        CANCEL_ARMED.with(|armed| armed.set(false));
        CANCEL_REQUESTS.with(|list| list.borrow_mut().clear());
        CANCEL_TICKETS.with(|tickets| tickets.borrow_mut().clear());
    }
}

/// 会话身份注入：`None` = 无活动会话。
struct SessionGuard;

impl SessionGuard {
    fn install(session: Option<&str>) -> Self {
        FORCED_SESSION.with(|slot| *slot.borrow_mut() = Some(session.map(str::to_string)));
        Self
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        FORCED_SESSION.with(|slot| *slot.borrow_mut() = None);
    }
}

// ── 全局 atoms / 面板栈的保存与恢复（断言失败也不污染其它用例）──────────

struct BgTasksGuard(Vec<BgTaskEntry>);

impl BgTasksGuard {
    fn install(items: Vec<BgTaskEntry>) -> Self {
        let previous = std::mem::take(&mut *BG_TASKS.state().write());
        *BG_TASKS.state().write() = items;
        Self(previous)
    }
}

impl Drop for BgTasksGuard {
    fn drop(&mut self) {
        *BG_TASKS.state().write() = std::mem::take(&mut self.0);
    }
}

struct CronJobsGuard(Vec<CronJobSummary>);

impl CronJobsGuard {
    fn install(items: Vec<CronJobSummary>) -> Self {
        let previous = std::mem::take(&mut *CRON_JOBS.state().write());
        *CRON_JOBS.state().write() = items;
        Self(previous)
    }
}

impl Drop for CronJobsGuard {
    fn drop(&mut self) {
        *CRON_JOBS.state().write() = std::mem::take(&mut self.0);
    }
}

struct ViewModelsGuard(crate::kit::atoms::ViewModelsSnapshot);

impl ViewModelsGuard {
    fn install(items: Vec<TuiRenderUnit>) -> Self {
        let previous = std::mem::take(&mut *VIEW_MODELS.state().write());
        *VIEW_MODELS.state().write() = crate::kit::atoms::ViewModelsSnapshot {
            items: im::Vector::from(items),
            generation: 0,
        };
        Self(previous)
    }
}

impl Drop for ViewModelsGuard {
    fn drop(&mut self) {
        *VIEW_MODELS.state().write() = std::mem::take(&mut self.0);
    }
}

/// 三段来源的空夹具（只留测试自备的后台任务段）：固定行与行号布局不依赖其它用例
/// 残留的 Cron / SubAgent，否则「可见项数 = 内容行 − 固定行」的断言会漂移。
struct TasksSourcesGuard {
    _cron: CronJobsGuard,
    _view_models: ViewModelsGuard,
}

impl TasksSourcesGuard {
    fn empty() -> Self {
        Self {
            _cron: CronJobsGuard::install(Vec::new()),
            _view_models: ViewModelsGuard::install(Vec::new()),
        }
    }
}

/// 面板栈：安装为「Tasks 打开且激活」——「保持打开」与「关闭」在断言上才有区别。
struct PanelStackGuard {
    open: Vec<PanelKind>,
    active: Option<PanelKind>,
}

impl PanelStackGuard {
    fn install() -> Self {
        let open = OPEN_PANELS.state().read().clone();
        let active = *ACTIVE_PANEL.state().read();
        *OPEN_PANELS.state().write() = vec![PanelKind::Tasks];
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Tasks);
        Self { open, active }
    }

    fn panel_is_open() -> bool {
        *ACTIVE_PANEL.state().read() == Some(PanelKind::Tasks)
    }
}

impl Drop for PanelStackGuard {
    fn drop(&mut self) {
        *OPEN_PANELS.state().write() = std::mem::take(&mut self.open);
        *ACTIVE_PANEL.state().write() = self.active;
    }
}

/// 状态栏通知的保存/恢复（`Notification` 不可 Clone，按原消息重新发布）。
struct NotificationGuard(Option<String>);

impl NotificationGuard {
    fn install() -> Self {
        let previous = NOTIFICATION
            .state()
            .read()
            .as_ref()
            .map(|notice| notice.message.clone());
        NOTIFICATION.set(None);
        Self(previous)
    }

    fn message() -> Option<String> {
        NOTIFICATION
            .state()
            .read()
            .as_ref()
            .map(|notice| notice.message.clone())
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        NOTIFICATION.set(self.0.take().map(|message| Notification {
            message,
            until: peri_time::monotonic_now() + Duration::from_secs(6),
        }));
    }
}

// ── 挂载脚手架 ─────────────────────────────────────────────────────────

pub(super) struct MountedPanel {
    pub(super) selected: State<usize>,
    pub(super) cancel_gates: State<CancelGates>,
    /// 组件上一帧的绘制区域（`AreaTracker`）——测试据此与渲染同源推导行号。
    pub(super) area: Option<Rect>,
}

/// 卸载时清空 visitor：断言 panic 也不把句柄泄漏给同线程的后续用例。
struct VisitorGuard;

impl Drop for VisitorGuard {
    fn drop(&mut self) {
        MOUNT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: RefCell<MountVisitor> = const { RefCell::new(None) };
}

/// 每帧把当前句柄交给 visitor（不消费）：第 N 帧的 visitor 观察到的是本帧更新阶段
/// （effect 收敛）之后的状态，故「改状态 → 断言效果」的用例至少渲染 2 帧。
pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 挂载面板并渲染 `frames` 帧；visitor 每帧收到 (帧序号, 当前句柄)，返回末帧缓冲。
fn mount_panel_frames(
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
        element!(super::TasksPanel()),
        TEST_WIDTH,
        TEST_HEIGHT,
        frames,
    )
}

/// 挂载 + 回传一次断言结果（跨帧用例经通道把结果带出 visitor）。
fn mount_and_act<T: 'static>(
    frames: usize,
    act: impl FnMut(usize, MountedPanel) -> Option<T> + 'static,
) -> (Buffer, Receiver<T>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut act = act;
    let buf = mount_panel_frames(frames, move |frame, panel| {
        if let Some(value) = act(frame, panel) {
            tx.send(value).expect("测试结果通道仍打开");
        }
    });
    (buf, rx)
}

impl MountedPanel {
    /// 当帧绘制区域（首帧 `AreaTracker` 未测量时退化为测试区域）。
    fn area(&self) -> Rect {
        self.area.unwrap_or(TEST_AREA)
    }

    /// 按当前 atom 内容推导当帧视口与快照后投喂事件——常规路径。
    fn event(&self, event: Event) -> EventResult {
        let (viewport, bg_tasks) = frame_state(*self.selected.read(), self.area());
        self.event_with(event, viewport, bg_tasks)
    }

    /// 走真实事件入口；`viewport` 与 `bg_tasks` 模拟「渲染该帧的事件闭包所持有的当帧
    /// 快照」（生产里闭包随渲染帧重建）。
    fn event_with(
        &self,
        event: Event,
        viewport: TasksViewport,
        bg_tasks: Vec<BgTaskEntry>,
    ) -> EventResult {
        super::handle_tasks_event(
            event,
            &super::TasksEventCtx {
                area: Some(self.area()),
                selected: self.selected,
                cancel_gates: self.cancel_gates,
                cancel_scope: current_scope(),
                viewport,
                bg_tasks,
            },
        )
    }
}

/// 当帧视口与后台任务快照——与组件渲染共用同一份推导与同一批 atom。
fn frame_state(selected: usize, area: Rect) -> (TasksViewport, Vec<BgTaskEntry>) {
    let bg_tasks: Vec<BgTaskEntry> = BG_TASKS.state().read().clone();
    let counts = SegmentCounts {
        background: bg_tasks.len(),
        cron: CRON_JOBS.state().read().len(),
        subagent: collect_subagents(&VIEW_MODELS.state().read()).len(),
    };
    let viewport =
        TasksViewport::for_content_rows(content_rows(Some(area), area.height), counts, selected);
    (viewport, bg_tasks)
}

// ── 夹具与构造 ─────────────────────────────────────────────────────────

fn bg_task(id: &str) -> BgTaskEntry {
    BgTaskEntry {
        task_id: id.to_string(),
        kind: "shell".to_string(),
        summary: format!("script {id}"),
        started_at: String::new(),
        pid: Some(42),
        revision: None,
        status: None,
    }
}

fn cron_job(id: &str) -> CronJobSummary {
    CronJobSummary {
        id: id.to_string(),
        expression: "0 * * * *".to_string(),
        prompt: format!("prompt {id}"),
        enabled: true,
        next_fire: None,
    }
}

fn subagent(id: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        instance_id: format!("instance-{id}"),
        agent_id: id.to_string(),
        agent_name: id.to_string(),
        parent_tool_call_id: None,
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: crate::kit::tui_render_unit::FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    })
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
        out.push_str(&buffer_row(buf, y));
        out.push('\n');
    }
    out
}

fn buffer_row(buf: &Buffer, y: u16) -> String {
    let mut out = String::new();
    for x in 0..buf.area.width {
        out.push_str(buf[(x, y)].symbol());
    }
    out
}

/// 夹具（8 后台 + 3 Cron + 2 SubAgent）下标 → 预期行身份。
fn expected_identity(index: usize) -> String {
    match index {
        0..8 => format!("bg{index}"),
        8..11 => format!("cron{}", index - 8),
        _ => format!("sa{}", index - 11),
    }
}

// ── 绘制与命中同源（挂载）──────────────────────────────────────────────

/// 每行都有独立、明确的文本状态标签；颜色不是唯一信息载体。
#[test]
fn background_task_rows_render_explicit_status_labels() {
    let theme = peri_theme::builtin::dark_theme();
    let cases = [
        (None, "bg-task-status-running"),
        (Some("running"), "bg-task-status-running"),
        (Some("lost"), "bg-task-status-lost"),
        (Some("reconciling"), "bg-task-status-reconciling"),
        (Some("pending_delivery"), "bg-task-status-pending-delivery"),
        (Some("delivery_pending"), "bg-task-status-pending-delivery"),
        (Some("completed"), "bg-task-status-completed"),
        (Some("failed"), "bg-task-status-failed"),
        (Some("cancelled"), "bg-task-status-cancelled"),
        (Some("future_status"), "bg-task-status-unknown"),
    ];
    let mut seen = std::collections::HashSet::new();
    for (status, key) in cases {
        let mut task = bg_task("bg-status");
        task.status = status.map(str::to_string);
        let rendered: String = background_task_line(&task, false, &theme)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let label = crate::i18n::tr(key);
        assert!(
            rendered.contains(&format!("[{label}]")),
            "状态 {status:?} 必须有文本标签 [{label}]：{rendered}"
        );
        assert!(
            rendered.contains("bg-status"),
            "完整 task_id 必须可定位：{rendered}"
        );
        seen.insert(label);
    }
    assert!(
        seen.len() >= 7,
        "不同状态必须产生不同标签（颜色仅辅助）：{seen:?}"
    );
}

/// 绘制窗口与鼠标命中同源：点中的行号命中的就是缓冲区里显示的那一项。
#[test]
#[serial]
fn drawn_rows_and_hit_testing_agree_across_segments() {
    let _bg = BgTasksGuard::install((0..8).map(|i| bg_task(&format!("bg{i}"))).collect());
    let _cron = CronJobsGuard::install((0..3).map(|i| cron_job(&format!("cron{i}"))).collect());
    let _vm = ViewModelsGuard::install(vec![subagent("sa0"), subagent("sa1")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-test"));

    let (buf, rx) = mount_and_act(2, move |frame, panel| {
        if frame == 0 {
            *panel.selected.write() = 6;
            return None;
        }
        assert_eq!(panel.area, Some(TEST_AREA), "事件区域须与绘制区域同源");
        let (viewport, bg_tasks) = frame_state(*panel.selected.read(), panel.area());
        let rows = viewport.rows().to_vec();

        // 先点所有非任务行：不选中、不关面板、不发请求
        let mut idle = Vec::new();
        for (row, kind) in rows.iter().enumerate() {
            if matches!(kind, TasksRow::Item(_)) {
                continue;
            }
            let before = *panel.selected.read();
            let result = panel.event_with(
                left_down(row as u16 + 1, 5),
                viewport.clone(),
                bg_tasks.clone(),
            );
            idle.push((row, before, *panel.selected.read(), result));
        }

        // 再逐个点任务行：命中的下标必须等于行表里该行的身份
        let mut hits = Vec::new();
        for (row, kind) in rows.iter().enumerate() {
            let TasksRow::Item(expected) = *kind else {
                continue;
            };
            let result = panel.event_with(
                left_down(row as u16 + 1, 5),
                viewport.clone(),
                bg_tasks.clone(),
            );
            hits.push((row, expected, *panel.selected.read(), result));
        }
        Some((rows, idle, hits))
    });

    let (rows, idle, hits) = rx.try_recv().expect("跨帧结果已送出");
    let text = buffer_text(&buf);

    assert_eq!(
        rows,
        vec![
            TasksRow::Summary,
            TasksRow::Blank,
            TasksRow::SectionHeader(TaskSegment::Background),
            TasksRow::Item(4),
            TasksRow::Item(5),
            TasksRow::Item(6),
            TasksRow::Item(7),
            TasksRow::Blank,
            TasksRow::SectionHeader(TaskSegment::Cron),
            TasksRow::Item(8),
            TasksRow::Item(9),
            TasksRow::Item(10),
            TasksRow::Blank,
            TasksRow::NavHint,
        ],
        "内容区 16 行、固定行 9 行 → 可见 7 项，SubAgent 段被裁掉"
    );
    assert!(
        text.contains("bg4") && text.contains("bg7"),
        "窗口内任务须绘制"
    );
    assert!(
        text.contains("cron0") && text.contains("cron2"),
        "窗口内的 Cron 段须绘制"
    );
    assert!(!text.contains("sa0"), "窗口外的段不得绘制：{text}");
    assert!(!text.contains("bg3"), "窗口之前的项不得绘制：{text}");

    for (row, before, after, result) in idle {
        assert_eq!(after, before, "第 {row} 行不是任务行，点击不得改变选中");
        assert_eq!(result, EventResult::Consumed, "面板内左键一律消费");
    }
    assert_eq!(
        CancelGuard::requests().len(),
        4,
        "窗口内 4 个后台任务行各发一次取消请求"
    );

    for (row, index, after, result) in hits {
        assert_eq!(after, index, "点击第 {row} 行必须选中该行");
        assert_eq!(result, EventResult::Consumed);
        let drawn = buffer_row(&buf, row as u16 + 1);
        let identity = expected_identity(index);
        assert!(
            drawn.contains(&identity),
            "命中的行身份与绘制不一致：第 {row} 行绘制 {drawn:?}，命中 {identity}"
        );
    }
}

/// 可见项数由面板内容行数推导，不写成常量。
#[test]
#[serial]
fn visible_items_follow_panel_height() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install((0..12).map(|i| bg_task(&format!("bg{i:02}"))).collect());
    let _panels = PanelStackGuard::install();

    // 首帧 `AreaTracker` 未测量（按 `panel_overlay` 预测行数），第 2 帧起用实测区域
    let short = mount_panel_frames(2, |_, _| {});
    let short_text = buffer_text(&short);
    assert!(short_text.contains("bg00"), "矮面板至少绘制第一项");
    assert!(
        short_text.contains("bg09") && !short_text.contains("bg10"),
        "内容区 16 行、固定行 6 行 → 只放得下 10 项：{short_text}"
    );

    let tall = ratatui_kit::test_util::render_frames(element!(super::TasksPanel()), 60, 40, 2);
    let tall_text = buffer_text(&tall);
    assert!(
        tall_text.contains("bg11"),
        "面板变高后必须绘制更多项，而不是停在按矮面板算出的窗口：{tall_text}"
    );
}

/// 列表刷新缩短列表后选中下标收敛，高亮行始终存在。
#[test]
#[serial]
fn selection_converges_when_list_shrinks() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install((0..5).map(|i| bg_task(&format!("bg{i}"))).collect());
    let _panels = PanelStackGuard::install();

    let (buf, rx) = mount_and_act(3, move |frame, panel| match frame {
        0 => {
            *panel.selected.write() = 4;
            None
        }
        1 => {
            // 任务完成 / 被取消：列表从 5 项缩到 2 项
            *BG_TASKS.state().write() = vec![bg_task("bg0"), bg_task("bg1")];
            None
        }
        _ => Some(*panel.selected.read()),
    });

    assert_eq!(
        rx.try_recv().ok(),
        Some(1),
        "越界选中必须收敛到缩短后的末项"
    );
    let text = buffer_text(&buf);
    assert!(text.contains("> [sh] bg1"), "高亮行必须存在：{text}");
    assert!(!text.contains("> [sh] bg0"), "高亮不得留在错误行：{text}");
}

// ── 取消动作的身份与可见结果 ───────────────────────────────────────────

/// Enter 取消的是用户看到的那一行：事件与渲染共用同一份当帧快照。
#[test]
#[serial]
fn enter_cancels_the_row_the_user_saw() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0"), bg_task("bg1"), bg_task("bg2")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));

    let (_, rx) = mount_and_act(2, move |frame, panel| {
        if frame == 0 {
            *panel.selected.write() = 2;
            return None;
        }
        // 用户看到的是当帧行表（bg2 高亮）；此刻活动列表已被刷新成另一组任务，
        // 事件闭包仍持当帧快照 → Enter 只能取消 bg2
        let (viewport, bg_tasks) = frame_state(*panel.selected.read(), panel.area());
        *BG_TASKS.state().write() = vec![bg_task("other0"), bg_task("other1"), bg_task("other2")];
        let result = panel.event_with(key(KeyCode::Enter), viewport, bg_tasks);
        Some((result, *panel.selected.read()))
    });

    let live_ids: Vec<String> = BG_TASKS
        .state()
        .read()
        .iter()
        .map(|task| task.task_id.clone())
        .collect();
    assert_eq!(
        live_ids,
        vec!["other0", "other1", "other2"],
        "对照：按键时刻的活动列表已经变了"
    );

    assert_eq!(
        CancelGuard::requests(),
        vec![(
            "s-1".to_string(),
            CancelTarget {
                task_id: "bg2".to_string(),
                kind: "shell".to_string(),
            }
        )],
        "取消目标必须是当帧渲染的那一行"
    );
    assert_eq!(rx.try_recv().ok(), Some((EventResult::Consumed, 2)));
    assert!(
        !PanelStackGuard::panel_is_open(),
        "请求已发出：沿用既有交互关闭面板"
    );
}

/// 活动列表在渲染之后刷新：行表里的身份不在快照里时不发请求，提示并保持面板打开。
#[test]
#[serial]
fn cancel_target_gone_notifies_and_keeps_panel_open() {
    let _bg = BgTasksGuard::install(vec![bg_task("bg0"), bg_task("bg1")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();

    mount_panel_frames(2, move |frame, panel| {
        if frame == 0 {
            return;
        }
        *panel.selected.write() = 1;
        // 当帧行表按 2 项渲染，而事件闭包拿到的快照只剩 1 项
        let viewport = TasksViewport::for_content_rows(
            TEST_CONTENT_ROWS,
            SegmentCounts {
                background: 2,
                ..SegmentCounts::default()
            },
            1,
        );
        panel.event_with(key(KeyCode::Enter), viewport, vec![bg_task("bg0")]);
    });

    assert!(CancelGuard::requests().is_empty(), "身份缺失时不得发请求");
    assert!(
        NotificationGuard::message().is_some(),
        "取消未发出必须给用户可见提示"
    );
    assert!(PanelStackGuard::panel_is_open(), "不得静默关闭面板");
}

/// 无活动会话时不发请求（旧实现用空串 sessionId 静默发出），提示并保持面板打开。
#[test]
#[serial]
fn cancel_without_session_notifies_and_keeps_panel_open() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0"), bg_task("bg1")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(None);
    let _notice = NotificationGuard::install();

    mount_panel_frames(2, move |frame, panel| {
        if frame == 1 {
            panel.event(key(KeyCode::Enter));
        }
    });

    assert!(CancelGuard::requests().is_empty(), "无会话时不得发请求");
    assert!(
        NotificationGuard::message().is_some(),
        "取消未发出必须给用户可见提示"
    );
    assert!(PanelStackGuard::panel_is_open(), "不得静默关闭面板");
}

/// Cron 行的 Enter 沿用 Enter::close：不发出取消请求但关闭面板。
#[test]
#[serial]
fn enter_on_cron_row_closes_without_cancel() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0")]);
    let _cron = CronJobsGuard::install(vec![cron_job("cron0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));

    mount_panel_frames(2, move |frame, panel| {
        if frame == 1 {
            *panel.selected.write() = 1;
            panel.event(key(KeyCode::Enter));
        }
    });

    assert!(
        CancelGuard::requests().is_empty(),
        "Cron 行不得发出取消请求"
    );
    assert!(
        !PanelStackGuard::panel_is_open(),
        "Cron 行的 Enter 语义是关闭面板"
    );
}

/// Esc 只关闭面板：不发送请求、不写通知。
#[test]
#[serial]
fn esc_only_closes_panel() {
    let _bg = BgTasksGuard::install(vec![bg_task("bg0")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));
    let _notice = NotificationGuard::install();

    mount_panel_frames(2, move |frame, panel| {
        if frame == 1 {
            panel.event(key(KeyCode::Esc));
        }
    });

    assert!(!PanelStackGuard::panel_is_open());
    assert!(CancelGuard::requests().is_empty());
    assert!(NotificationGuard::message().is_none());
}

// ── 鼠标语义（click as enter）───────────────────────────────────────────

/// 点击任务行 = 选中该行并执行其 Enter 语义。
#[test]
#[serial]
fn click_on_background_row_selects_and_cancels_it() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0"), bg_task("bg1"), bg_task("bg2")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));

    let (buf, rx) = mount_and_act(2, move |frame, panel| {
        if frame == 0 {
            return None;
        }
        // 内容行 0 起：摘要、空行、段标题、bg0、bg1、bg2 → bg1 在内容行 4
        let result = panel.event(left_down(5, 5));
        Some((result, *panel.selected.read()))
    });

    assert_eq!(
        rx.try_recv().ok(),
        Some((EventResult::Consumed, 1)),
        "点击选中被点的那一行"
    );
    assert_eq!(
        CancelGuard::requests(),
        vec![(
            "s-1".to_string(),
            CancelTarget {
                task_id: "bg1".to_string(),
                kind: "shell".to_string(),
            }
        )]
    );
    assert!(buffer_text(&buf).contains("bg1"));
    assert!(
        !PanelStackGuard::panel_is_open(),
        "click as enter：与 Enter 同语义"
    );
}

/// 滚动条列不参与命中。
#[test]
#[serial]
fn click_on_scrollbar_column_does_not_act() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0"), bg_task("bg1"), bg_task("bg2")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));

    let (_, rx) = mount_and_act(2, move |frame, panel| {
        if frame == 0 {
            return None;
        }
        let result = panel.event(left_down(5, TEST_WIDTH - 1));
        Some((result, *panel.selected.read()))
    });

    assert_eq!(rx.try_recv().ok(), Some((EventResult::Consumed, 0)));
    assert!(CancelGuard::requests().is_empty(), "滚动条列不得触发取消");
    assert!(PanelStackGuard::panel_is_open(), "滚动条列不得关闭面板");
}

/// 面板内其他左键消费但不动作；非左键忽略。
#[test]
#[serial]
fn click_outside_rows_consumes_without_action() {
    let _sources = TasksSourcesGuard::empty();
    let _bg = BgTasksGuard::install(vec![bg_task("bg0"), bg_task("bg1")]);
    let _panels = PanelStackGuard::install();
    let _cancel = CancelGuard::install();
    let _session = SessionGuard::install(Some("s-1"));

    let (_, rx) = mount_and_act(2, move |frame, panel| {
        if frame == 0 {
            return None;
        }
        // 内容行 0/1 = 摘要与空行，内容行 2 = 段标题
        let header = panel.event(left_down(3, 5));
        let scrolled = panel.event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        Some((header, scrolled, *panel.selected.read()))
    });

    assert_eq!(
        rx.try_recv().ok(),
        Some((EventResult::Consumed, EventResult::Ignored, 0))
    );
    assert!(CancelGuard::requests().is_empty());
    assert!(PanelStackGuard::panel_is_open(), "非任务行不得关闭面板");
}
