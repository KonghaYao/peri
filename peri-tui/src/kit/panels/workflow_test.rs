//! WorkflowPanel 回归：选中身份（稳定 run id）随快照收敛、终止请求的身份与守卫。
//!
//! [Why] 事件入口依赖 `State<T>` 句柄（无公开构造 API），故经挂载探针
//! `visit_mounted_panel` 从真实挂载中捕获句柄后投喂事件；终止请求与会话身份由测试接缝
//! 注入——单测进程里 `ACP_CLIENT_HANDLE` 由其它用例的 `OnceLock` 决定，构造不出可控的
//! 会话环境。

use std::cell::{Cell, RefCell};

use super::*;
use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
use crate::kit::workflow_snapshot::WorkflowSnapshot;
use ratatui_kit::crossterm::event::{KeyEvent, KeyModifiers};
use ratatui_kit::ratatui::buffer::Buffer;
use serial_test::serial;

// ── 终止请求的测试接缝 ──────────────────────────────────────────────────

thread_local! {
    /// 已记录的终止请求：(会话, run)。
    static KILL_REQUESTS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// 接缝是否接管终止请求。
    static KILL_ARMED: Cell<bool> = const { Cell::new(false) };
}

/// 测试接缝：记录终止请求并阻止真实发送；未接管时返回 false（走真实发送分支）。
pub(super) fn intercept_kill_request(session_id: &str, run_id: &str) -> bool {
    if !KILL_ARMED.with(Cell::get) {
        return false;
    }
    KILL_REQUESTS.with(|list| {
        list.borrow_mut()
            .push((session_id.to_string(), run_id.to_string()))
    });
    true
}

/// 终止请求记录器：安装期间请求被记录而不发送。
struct KillGuard;

impl KillGuard {
    fn install() -> Self {
        KILL_REQUESTS.with(|list| list.borrow_mut().clear());
        KILL_ARMED.with(|armed| armed.set(true));
        Self
    }

    fn requests() -> Vec<(String, String)> {
        KILL_REQUESTS.with(|list| list.borrow().clone())
    }
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        KILL_ARMED.with(|armed| armed.set(false));
        KILL_REQUESTS.with(|list| list.borrow_mut().clear());
    }
}

// ── atom / 面板栈的保存与恢复（断言失败也不污染其它用例）────────────────

/// 面板相关 atom 与面板栈的保存/恢复。
struct WorkflowGuard {
    snapshot: Option<WorkflowSnapshot>,
    selected_run: Option<String>,
    session: String,
    reset: u64,
    open_panels: Vec<PanelKind>,
    active_panel: Option<PanelKind>,
}

impl WorkflowGuard {
    /// 安装夹具：`runs` 为轮询写回的当前快照；`session` 为 `None` 表示无活动会话。
    fn install(runs: Vec<TuiRunProgress>, session: Option<&str>) -> Self {
        let previous = Self {
            snapshot: std::mem::take(&mut *WORKFLOW_SNAPSHOT.state().write()),
            selected_run: std::mem::take(&mut *SELECTED_WORKFLOW_RUN_ID.state().write()),
            session: ACTIVE_SESSION_ID.state().read().clone(),
            reset: BRIDGE_RESET_COUNTER.get(),
            open_panels: std::mem::take(&mut *OPEN_PANELS.state().write()),
            active_panel: std::mem::take(&mut *ACTIVE_PANEL.state().write()),
        };
        Self::set_runs(runs);
        ACTIVE_SESSION_ID.set(session.unwrap_or_default().to_string());
        BRIDGE_RESET_COUNTER.set(0);
        *OPEN_PANELS.state().write() = vec![PanelKind::Workflow];
        *ACTIVE_PANEL.state().write() = Some(PanelKind::Workflow);
        previous
    }

    /// 轮询写回快照——面板下一次渲染才看到新列表。
    fn set_runs(runs: Vec<TuiRunProgress>) {
        *WORKFLOW_SNAPSHOT.state().write() = Some(WorkflowSnapshot { runs });
    }
}

impl Drop for WorkflowGuard {
    fn drop(&mut self) {
        *WORKFLOW_SNAPSHOT.state().write() = self.snapshot.take();
        *SELECTED_WORKFLOW_RUN_ID.state().write() = self.selected_run.take();
        ACTIVE_SESSION_ID.set(std::mem::take(&mut self.session));
        BRIDGE_RESET_COUNTER.set(self.reset);
        *OPEN_PANELS.state().write() = std::mem::take(&mut self.open_panels);
        *ACTIVE_PANEL.state().write() = self.active_panel.take();
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

    /// 当前通知文案——终止请求的结果必须对用户可见，不能只留日志。
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
            until: peri_time::monotonic_now() + Duration::from_secs(2),
        }));
    }
}

// ── 挂载脚手架 ─────────────────────────────────────────────────────────

type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: RefCell<MountVisitor> = const { RefCell::new(None) };
}

/// 卸载时清空 visitor：断言 panic 也不把句柄泄漏给同线程的后续用例。
struct VisitorGuard;

impl Drop for VisitorGuard {
    fn drop(&mut self) {
        MOUNT_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

/// 挂载探针交出的句柄（`State<T>` 无公开构造 API）。
pub(super) struct MountedPanel {
    pub(super) selected_run_id: State<Option<String>>,
    pub(super) focus_left: State<bool>,
    pub(super) phase_sel: State<usize>,
    pub(super) agent_sel: State<usize>,
    pub(super) kill_gate: State<CompletionGate>,
}

pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 测试面板尺寸（含 `panel_shell!` 边框）。
const TEST_WIDTH: u16 = 60;
const TEST_HEIGHT: u16 = 18;

/// 挂载面板并渲染 `frames` 帧；visitor 每帧收到 (帧序号, 当前句柄)，返回末帧缓冲。
///
/// effect 在组件体内同步执行、先于 visitor：第 N 帧 visitor 观察到的是本帧更新阶段
/// 收敛后的状态，故「改状态 → 断言效果」的用例至少渲染 2 帧。
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
        element!(super::WorkflowPanel()),
        TEST_WIDTH,
        TEST_HEIGHT,
        frames,
    )
}

impl MountedPanel {
    /// 走真实事件入口；run 身份列表取当前快照——模拟「渲染该帧的事件闭包所持有的当帧
    /// 快照」（生产里闭包随渲染帧重建）。
    fn event(&self, event: Event) -> EventResult {
        let run_ids: Vec<String> = WORKFLOW_SNAPSHOT
            .state()
            .read()
            .as_ref()
            .map(|snapshot| snapshot.runs.iter().map(|run| run.run_id.clone()).collect())
            .unwrap_or_default();
        handle_workflow_event(
            event,
            &WorkflowEventCtx {
                run_ids: &run_ids,
                selected_run_id: self.selected_run_id,
                focus_left: self.focus_left,
                phase_sel: self.phase_sel,
                agent_sel: self.agent_sel,
                kill_gate: self.kill_gate,
            },
        )
    }

    /// 选中身份（稳定 run id）。
    fn selected(&self) -> Option<String> {
        self.selected_run_id.read().clone()
    }
}

// ── 夹具与构造 ─────────────────────────────────────────────────────────

/// 一条 run 的最小快照条目；phase 标题用于断言「面板选中的是哪条 run」。
fn run(id: &str) -> TuiRunProgress {
    TuiRunProgress {
        run_id: id.to_string(),
        workflow_name: format!("wf-{id}"),
        status: "running".to_string(),
        phases: vec![crate::kit::workflow_snapshot::TuiPhaseProgress {
            title: format!("phase-{id}"),
            status: "active".to_string(),
        }],
        ..Default::default()
    }
}

/// 两 phase / 两 agent 的 run——列内导航需要可移动的空间。
fn rich_run(id: &str) -> TuiRunProgress {
    let phase = format!("phase-{id}-1");
    let agent = |agent_id: u64, status: &str| crate::kit::workflow_snapshot::TuiAgentProgress {
        agent_id,
        phase: Some(phase.clone()),
        status: status.to_string(),
        ..Default::default()
    };
    TuiRunProgress {
        run_id: id.to_string(),
        workflow_name: format!("wf-{id}"),
        status: "running".to_string(),
        phases: vec![
            crate::kit::workflow_snapshot::TuiPhaseProgress {
                title: phase.clone(),
                status: "active".to_string(),
            },
            crate::kit::workflow_snapshot::TuiPhaseProgress {
                title: format!("phase-{id}-2"),
                status: "pending".to_string(),
            },
        ],
        agents: vec![agent(1, "running"), agent(2, "done")],
        ..Default::default()
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
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

/// 提示文案的前几字——面板宽度可能截断长文案，取前缀断言「该提示可见」。
fn hint_prefix(key: &str) -> String {
    i18n::tr(key).chars().take(6).collect()
}

// ── 选中身份：run id（不是下标）────────────────────────────────────────

/// [回归] 快照重排后选中项跟随 run id：外部选中 a 之后重排，仍选中 a。
#[test]
#[serial]
fn selection_follows_run_id_after_snapshot_reorder() {
    let _guard = WorkflowGuard::install(vec![run("a"), run("b"), run("c")], Some("s-test"));
    let _kill = KillGuard::install();
    *SELECTED_WORKFLOW_RUN_ID.state().write() = Some("a".into());

    let buf = mount_panel_frames(3, |frame, panel| {
        if frame != 0 {
            return;
        }
        assert_eq!(panel.selected(), Some("a".to_string()), "外部选中先落地");
        // 轮询把同一条 run 换到末尾（下标全变）
        WorkflowGuard::set_runs(vec![run("b"), run("c"), run("a")]);
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains("phase-a"),
        "选中列应仍显示 a 的 phase：{text}"
    );
    assert!(
        !text.contains("phase-b"),
        "不得落到同下标的另一条 run：{text}"
    );
}

/// [回归] `SELECTED_WORKFLOW_RUN_ID == None` 时快照重排：选中仍指向原来的 run。
///
/// 旧实现按下标收敛，重排后 `active_run` 会静默指向同下标的另一条 run。
#[test]
#[serial]
fn default_selection_follows_run_id_after_snapshot_reorder() {
    let _guard = WorkflowGuard::install(vec![run("a"), run("b"), run("c")], Some("s-test"));
    let _kill = KillGuard::install();

    let buf = mount_panel_frames(3, |frame, panel| {
        if frame != 0 {
            return;
        }
        assert_eq!(panel.selected(), Some("a".to_string()), "首帧落到首项");
        WorkflowGuard::set_runs(vec![run("b"), run("c"), run("a")]);
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains("phase-a"),
        "选中列应仍显示 a 的 phase：{text}"
    );
    assert!(
        !text.contains("phase-b"),
        "不得落到同下标的另一条 run：{text}"
    );
}

/// [回归] 用户 Tab 选中的 run 在快照重排后仍是同一条。
#[test]
#[serial]
fn tab_selected_run_follows_id_after_reorder() {
    let _guard = WorkflowGuard::install(vec![run("a"), run("b"), run("c")], Some("s-test"));
    let _kill = KillGuard::install();

    let buf = mount_panel_frames(3, |frame, panel| {
        match frame {
            0 => {
                // Tab 落到此刻下标 1 的 run b
                assert_eq!(panel.event(key(KeyCode::Tab)), EventResult::Consumed);
                assert_eq!(panel.selected(), Some("b".to_string()));
                // 重排：b 挪到末位（下标 1 上换成了 a）
                WorkflowGuard::set_runs(vec![run("c"), run("a"), run("b")]);
            }
            2 => assert_eq!(
                panel.selected(),
                Some("b".to_string()),
                "选中身份不得随下标漂移"
            ),
            _ => {}
        }
    });

    let text = buffer_text(&buf);
    assert!(text.contains("phase-b"), "选中列应显示 b 的 phase：{text}");
    assert!(
        !text.contains("phase-a"),
        "不得落到同下标的另一条 run：{text}"
    );
}

/// 外部选择只应用一次：用户 Tab 切走后，同一份外部选择不会把他拉回。
#[test]
#[serial]
fn applied_external_selection_does_not_pull_user_back() {
    let _guard = WorkflowGuard::install(vec![run("a"), run("b")], Some("s-test"));
    let _kill = KillGuard::install();
    *SELECTED_WORKFLOW_RUN_ID.state().write() = Some("a".into());

    mount_panel_frames(3, |frame, panel| match frame {
        0 => assert_eq!(panel.selected(), Some("a".to_string()), "外部选中先落地"),
        1 => {
            assert_eq!(panel.event(key(KeyCode::Tab)), EventResult::Consumed);
            assert_eq!(panel.selected(), Some("b".to_string()));
        }
        2 => assert_eq!(
            panel.selected(),
            Some("b".to_string()),
            "外部选择不得把用户拉回"
        ),
        _ => {}
    });
}

/// [回归] 外部选中 id 不在快照里：提示「未同步」+ 落到首项（既有提示语义）。
#[test]
#[serial]
fn external_selection_missing_hints_and_clamps_to_first() {
    let _guard = WorkflowGuard::install(vec![run("a"), run("b")], Some("s-test"));
    let _kill = KillGuard::install();
    *SELECTED_WORKFLOW_RUN_ID.state().write() = Some("gone".into());

    let buf = mount_panel_frames(3, |frame, panel| {
        if frame == 2 {
            assert_eq!(
                panel.selected(),
                Some("a".to_string()),
                "身份缺失时落到首项"
            );
        }
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains(&hint_prefix(RUN_NOT_SYNCED_KEY)),
        "未同步提示应对用户可见：{text}"
    );
    assert!(text.contains("phase-a"), "落点应是首项：{text}");
}

/// Esc 关面板——键位与既有语义不变（面板栈弹栈）。
#[test]
#[serial]
fn esc_closes_panel() {
    let _guard = WorkflowGuard::install(vec![run("a")], Some("s-test"));
    let _kill = KillGuard::install();

    mount_panel_frames(2, |frame, panel| {
        if frame == 1 {
            assert_eq!(panel.event(key(KeyCode::Esc)), EventResult::Consumed);
        }
    });

    assert_eq!(*ACTIVE_PANEL.state().read(), None, "Esc 应关闭面板");
}

/// 导航键位与既有语义不变：←/→ 切左右焦点，↑/↓ 在列内移动，Shift+Tab 反向切 run。
#[test]
#[serial]
fn nav_keys_keep_existing_semantics() {
    let _guard = WorkflowGuard::install(
        vec![rich_run("a"), rich_run("b"), rich_run("c")],
        Some("s-test"),
    );
    let _kill = KillGuard::install();

    mount_panel_frames(2, |frame, panel| {
        if frame != 0 {
            return;
        }
        // 默认焦点在左列（phases）
        assert_eq!(panel.event(key(KeyCode::Down)), EventResult::Consumed);
        assert_eq!(*panel.phase_sel.read(), 1);
        assert_eq!(panel.event(key(KeyCode::Up)), EventResult::Consumed);
        assert_eq!(*panel.phase_sel.read(), 0);

        // → 切到右列，↓/↑ 改的是 agent 选择
        assert_eq!(panel.event(key(KeyCode::Right)), EventResult::Consumed);
        assert!(!*panel.focus_left.read());
        assert_eq!(panel.event(key(KeyCode::Down)), EventResult::Consumed);
        assert_eq!(*panel.agent_sel.read(), 1);
        assert_eq!(panel.event(key(KeyCode::Left)), EventResult::Consumed);
        assert!(*panel.focus_left.read());

        // Shift+Tab（BackTab）反向切 run：默认选中首项，反向绕到末项，列内选择回到首项
        *panel.phase_sel.write() = 1;
        assert_eq!(panel.event(key(KeyCode::BackTab)), EventResult::Consumed);
        assert_eq!(panel.selected(), Some("c".to_string()));
        assert_eq!(*panel.phase_sel.read(), 0);
        assert_eq!(*panel.agent_sel.read(), 0);
    });
}

// ── 终止运行：身份与守卫 ───────────────────────────────────────────────

/// 终止请求按稳定 run id 发出，会话身份取自当前会话边界。
#[test]
#[serial]
fn kill_sends_selected_run_id_under_current_session() {
    let _guard = WorkflowGuard::install(vec![run("a"), run("b")], Some("s-test"));
    let _kill = KillGuard::install();
    let _notify = NotificationGuard::install();

    mount_panel_frames(3, |frame, panel| match frame {
        0 => {
            assert_eq!(panel.event(key(KeyCode::Tab)), EventResult::Consumed);
        }
        2 => {
            assert_eq!(panel.event(key(KeyCode::Enter)), EventResult::Consumed);
        }
        _ => {}
    });

    assert_eq!(
        KillGuard::requests(),
        vec![("s-test".to_string(), "b".to_string())]
    );
    assert_eq!(
        NotificationGuard::message(),
        None,
        "响应未返回前不得报告已请求终止"
    );
}

/// [回归] 无活动会话时终止请求不得发出——旧实现发空串 sessionId，用户看不到原因。
#[test]
#[serial]
fn kill_without_active_session_notifies_and_sends_nothing() {
    let _guard = WorkflowGuard::install(vec![run("a")], None);
    let _kill = KillGuard::install();
    let _notify = NotificationGuard::install();

    mount_panel_frames(2, |frame, panel| {
        if frame == 1 {
            assert_eq!(panel.event(key(KeyCode::Enter)), EventResult::Consumed);
        }
    });

    assert!(KillGuard::requests().is_empty(), "无会话不得发出终止请求");
    assert_eq!(
        NotificationGuard::message(),
        Some(i18n::tr(KILL_NO_SESSION_KEY))
    );
}

/// [回归] 目标 run 已不在当前快照里：放弃发送 + 提示，不按旧下标取「现在这一项」。
#[test]
#[serial]
fn kill_target_gone_from_snapshot_notifies_and_sends_nothing() {
    let _guard = WorkflowGuard::install(vec![run("a")], Some("s-test"));
    let _kill = KillGuard::install();
    let _notify = NotificationGuard::install();

    mount_panel_frames(2, |frame, panel| {
        if frame != 0 {
            return;
        }
        assert_eq!(panel.selected(), Some("a".to_string()));
        // 事件到达前轮询已把 a 淘汰：身份还在，快照里已没有它
        WorkflowGuard::set_runs(vec![run("b")]);
        assert_eq!(panel.event(key(KeyCode::Enter)), EventResult::Consumed);
    });

    assert!(
        KillGuard::requests().is_empty(),
        "目标已消失不得发出终止请求"
    );
    assert_eq!(
        NotificationGuard::message(),
        Some(i18n::tr(RUN_NOT_SYNCED_KEY))
    );
}

// ── 纯函数 ─────────────────────────────────────────────────────────────

/// [单测] 身份命中取该 run 的下标。
#[test]
fn test_selected_run_index_selects_matching_run() {
    let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert_eq!(selected_run_index(&ids, Some("b")), 1);
    assert_eq!(selected_run_index(&ids, Some("a")), 0);
    assert_eq!(selected_run_index(&ids, Some("gone")), 0);
    assert_eq!(selected_run_index(&ids, None), 0);
}

/// [回归测试] 快照收缩 / 重排后选中落点由身份推导：不越界，也不漂移到同下标的另一条 run。
///
/// 历史背景：后台 workflow 回调后的快照从多条 run 收缩为一条时，`WorkflowPanel`
/// 直接以旧下标索引列表，导致 TUI panic；此后改为按下标 clamp，快照重排会让选中
/// 静默落到同下标的另一条 run（挂载用例覆盖）。空列表时返回的 0 是占位值——
/// 渲染与事件判定都在取用前先判空。
#[test]
fn test_selected_run_index_after_snapshot_shrinks() {
    let ids = vec!["only".to_string()];
    assert_eq!(selected_run_index(&ids, Some("gone")), 0);

    let reordered = vec!["b".to_string(), "a".to_string()];
    assert_eq!(selected_run_index(&reordered, Some("a")), 1);

    assert_eq!(selected_run_index(&[], Some("a")), 0);
}

#[test]
fn test_agent_table_header_model_column_aligns_with_row_prefix() {
    let header = agent_table_header("Model");
    let model_start = header.find("Model").expect("model header");
    assert_eq!(
        model_start,
        super::AGENT_ROW_LEADING_COLS + super::AGENT_NAME_COL_WIDTH
    );
}

/// [单测] Model 列 Unicode 宽度截断 helper：ASCII/CJK 均按终端显示宽度
/// 截断（CJK 每字符 2 列），超宽时以 '…'（1 列）收尾。
#[test]
fn test_truncate_to_width_unicode_safe() {
    assert_eq!(truncate_to_width("short", 12), "short");
    assert_eq!(truncate_to_width("claude-sonnet-4-5", 12), "claude-sonn…");
    assert_eq!(truncate_to_width("中文模型名", 6), "中文…");
    assert_eq!(truncate_to_width("ab", 1), "…");
}

/// [单测] Model 列单元格：缺失显示 '-'，未超宽补齐到列宽，超宽截断。
#[test]
fn test_model_cell_missing_padding_and_truncate() {
    assert_eq!(model_cell(None, 12), format!("-{}", " ".repeat(11)));
    assert_eq!(model_cell(Some("claude-haiku"), 12), "claude-haiku");
    assert_eq!(
        model_cell(Some("haiku"), 12),
        format!("haiku{}", " ".repeat(7))
    );
    assert_eq!(model_cell(Some("claude-sonnet-4-5"), 12), "claude-sonn…");
}
