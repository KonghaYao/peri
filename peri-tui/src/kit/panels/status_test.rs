//! StatusPanel 面板测试：页签切换（hook 顺序跨帧）、空态/错误态可见与统计取值。

use super::*;

use crate::kit::atoms::{McpInitPhase, McpStatusSnapshot, ServiceSnapshot, ViewModelsSnapshot};
use crate::kit::tui_render_unit::{TuiSubAgentGroup, TuiSystemNote, TuiUserBubble};
use ratatui_kit::crossterm::event::{KeyEvent, KeyModifiers};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use serial_test::serial;

/// `SERVICE_SNAPSHOT` 的保存/恢复——断言失败也不污染其它测试。
struct ServiceInfoGuard(ServiceSnapshot);

impl ServiceInfoGuard {
    fn install(snapshot: ServiceSnapshot) -> Self {
        let previous = std::mem::take(&mut *SERVICE_SNAPSHOT.state().write());
        *SERVICE_SNAPSHOT.state().write() = snapshot;
        Self(previous)
    }
}

impl Drop for ServiceInfoGuard {
    fn drop(&mut self) {
        *SERVICE_SNAPSHOT.state().write() = std::mem::take(&mut self.0);
    }
}

/// `VIEW_MODELS` 的保存/恢复——断言失败也不污染其它测试。
struct ViewModelsGuard(ViewModelsSnapshot);

impl ViewModelsGuard {
    fn install(items: Vec<TuiRenderUnit>) -> Self {
        let previous = std::mem::take(&mut *VIEW_MODELS.state().write());
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
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
    pub(super) active_tab: State<usize>,
}

impl MountedPanel {
    fn event(&self, event: Event) -> EventResult {
        handle_status_event(event, self.active_tab)
    }
}

pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 挂载并渲染 `frames` 帧，返回末帧缓冲；句柄逐帧交给 visitor。
///
/// `height` 是测试缓冲高度：面板在真实布局里固定 60×18，此处放高可以看清被
/// 18 行窗口裁掉的尾部内容行（面板自身无滚动能力，裁剪属于现状）。
fn mount_panel_frames(
    height: u16,
    frames: usize,
    visitor: impl FnMut(MountedPanel) + 'static,
) -> Buffer {
    let _guard = VisitorGuard;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    ratatui_kit::test_util::render_frames(element!(super::StatusPanel()), 60, height, frames)
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

fn user_bubble(text: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: text.to_string(),
        attachments: Vec::new(),
        content_hash: 0,
        reminder: None,
        source: None,
        message_id: None,
    })
}

fn system_note(text: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: text.to_string(),
        level: crate::kit::tui_render_unit::TuiNoteLevel::Info,
        content_hash: 0,
    })
}

fn subagent_group(id: &str) -> TuiSubAgentGroup {
    TuiSubAgentGroup {
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
    }
}

/// 空态不被吞掉：未配置 provider/model 时渲染空态文案，MCP 失败阶段照常显示。
#[test]
#[serial]
fn unconfigured_snapshot_shows_empty_labels() {
    let _snap = ServiceInfoGuard::install(ServiceSnapshot {
        mcp: McpStatusSnapshot {
            init_phase: McpInitPhase::Failed,
            total: 3,
            connected: 0,
        },
        ..ServiceSnapshot::default()
    });
    let buf = mount_panel_frames(18, 2, |_| {});
    let text = buffer_text(&buf);
    assert!(
        text.contains(&i18n::tr("app-not-configured")),
        "provider 未配置必须显示空态文案：\n{text}"
    );
    assert!(
        text.contains(&i18n::tr("app-empty")),
        "model 未配置必须显示空态文案：\n{text}"
    );
    assert!(
        text.contains("failed") && text.contains("0/3 connected"),
        "MCP 失败阶段与连接数必须可见：\n{text}"
    );
}

/// Service 页签渲染快照里的真实取值（不被空态或占位吞掉）。
#[test]
#[serial]
fn service_tab_renders_snapshot_values() {
    let _snap = ServiceInfoGuard::install(ServiceSnapshot {
        cwd: "/tmp/peri".to_string(),
        provider_name: "anthropic".to_string(),
        model_alias: "sonnet".to_string(),
        permission_mode: "plan".to_string(),
        cpu_percent: 42.5,
        memory_mb: 512,
        mcp: McpStatusSnapshot {
            init_phase: McpInitPhase::Ready,
            total: 3,
            connected: 2,
        },
        cron_total: 4,
        cron_enabled: 2,
        ..ServiceSnapshot::default()
    });
    // 放高缓冲：真实面板固定 18 行，内容超出的尾部行会被裁掉（本面板无滚动能力），
    // 这里只验证「快照取值都进了渲染内容」，不锁定裁剪现状。
    let buf = mount_panel_frames(40, 2, |_| {});
    let text = buffer_text(&buf);
    for needle in [
        "anthropic",
        "sonnet",
        "plan",
        "/tmp/peri",
        "42.5%",
        "512 MB",
        "2/3 connected",
        "ready",
        "4 (2 enabled)",
    ] {
        assert!(
            text.contains(needle),
            "Service 页签必须渲染 {needle}：\n{text}"
        );
    }
    assert!(
        !text.contains(&i18n::tr("status-label-total-vms")),
        "Service 页签不得渲染 Context 页签内容：\n{text}"
    );
}

/// 页签切换是可观察行为：→ 切到 Context 并渲染其内容，← 切回 Service。
/// 同一实例内跨帧切换分支也验证 hook 序列稳定（TUI-HOOK-001）。
#[test]
#[serial]
fn tab_keys_switch_content_and_keep_hook_sequence() {
    let _snap = ServiceInfoGuard::install(ServiceSnapshot::default());
    let _vm = ViewModelsGuard::install(vec![
        user_bubble("hi"),
        user_bubble("again"),
        system_note("note"),
        TuiRenderUnit::TuiSubAgentGroup(subagent_group("researcher")),
    ]);

    // `frame` 是 Copy，move 进回调的是副本——外层计数必须经 `Rc<Cell>` 观察
    let rendered = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = rendered.clone();
    let mut frame = 0usize;
    let buf = mount_panel_frames(18, 4, move |panel| {
        match frame {
            1 => {
                assert_eq!(panel.event(key(KeyCode::Right)), EventResult::Consumed);
                assert_eq!(*panel.active_tab.read(), TAB_CONTEXT, "→ 切到 Context");
            }
            2 => {
                assert_eq!(panel.event(key(KeyCode::Left)), EventResult::Consumed);
                assert_eq!(*panel.active_tab.read(), TAB_SERVICE, "← 切回 Service");
            }
            _ => {}
        }
        frame += 1;
        counter.set(counter.get() + 1);
    });
    assert!(
        rendered.get() >= 3,
        "面板必须在页签切换后继续渲染，实际 {} 帧",
        rendered.get()
    );
    let text = buffer_text(&buf);
    assert!(
        text.contains(&i18n::tr("status-label-cpu")),
        "末帧位于 Service 页签：\n{text}"
    );
    assert!(
        !text.contains(&i18n::tr("status-label-total-vms")),
        "末帧不得残留 Context 页签内容：\n{text}"
    );
}

/// Context 页签按渲染单元类型统计，计数不是写死的占位。
#[test]
#[serial]
fn context_tab_renders_derived_vm_counts() {
    let _snap = ServiceInfoGuard::install(ServiceSnapshot::default());
    let _vm = ViewModelsGuard::install(vec![
        user_bubble("hi"),
        user_bubble("again"),
        system_note("note"),
        TuiRenderUnit::TuiSubAgentGroup(subagent_group("researcher")),
    ]);

    let buf = mount_panel_frames(18, 3, |panel| {
        *panel.active_tab.write() = TAB_CONTEXT;
    });
    let text = buffer_text(&buf);
    assert!(
        text.contains(&i18n::tr("status-label-total-vms")),
        "Context 页签内容必须渲染：\n{text}"
    );
    assert!(
        text.contains(&i18n::tr("status-label-subagent-groups")),
        "Context 页签必须渲染 SubAgent 分组计数：\n{text}"
    );
}

/// 统计函数按渲染单元类型分类，且递归计数分组内的渲染单元。
#[test]
fn derive_vm_stats_counts_each_kind_recursively() {
    let mut group = subagent_group("researcher");
    group.view_models = im::Vector::from(vec![user_bubble("inner")]);
    let snap = ViewModelsSnapshot {
        items: im::Vector::from(vec![
            user_bubble("hi"),
            system_note("note"),
            TuiRenderUnit::TuiSubAgentGroup(group),
        ]),
        generation: 0,
    };

    let stats = derive_vm_stats(&snap);
    assert_eq!(
        stats.user_bubbles, 2,
        "外层 user bubble + 分组内 user bubble"
    );
    assert_eq!(stats.system_notes, 1);
    assert_eq!(stats.subagent_groups, 1);
    assert_eq!(stats.assistant_bubbles, 0);
    assert_eq!(stats.tool_cards, 0);
    assert_eq!(stats.total, 4, "total 必须等于各类之和");
}
