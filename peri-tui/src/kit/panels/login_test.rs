//! Tests for LoginPanel component.
//!
//! 面板级用例走真实事件入口：`State<T>` 无公开构造 API，句柄从挂载探针捕获
//! （visitor 模式，同 memory/mcp 面板测试）。删除请求经
//! `intercept_delete_request` 接缝记录——测试进程的 `PERI_CONFIG_HANDLE` 是
//! 不可安装的进程级 `OnceLock`，真实删除路径（配置改写 + 落盘 + ACP 推送）不在
//! 单测覆盖范围内。

use super::*;

use crate::kit::atoms::PROVIDER_LIST;
use ratatui_kit::crossterm::event::{Event, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;

/// 测试面板区域：整屏 80×24（组件直接渲染进缓冲，组件区域即整屏）。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

fn provider(id: &str) -> ProviderSummary {
    ProviderSummary {
        id: id.to_string(),
        provider_type: "anthropic".to_string(),
        is_active: false,
        has_api_key: true,
        base_url: None,
    }
}

/// 投影列表的保存/恢复——断言失败也不污染其它测试。
struct ProviderListGuard(Vec<ProviderSummary>);

impl ProviderListGuard {
    fn install(items: Vec<ProviderSummary>) -> Self {
        let previous = PROVIDER_LIST.state().read().clone();
        *PROVIDER_LIST.state().write() = items;
        Self(previous)
    }
}

impl Drop for ProviderListGuard {
    fn drop(&mut self) {
        *PROVIDER_LIST.state().write() = std::mem::take(&mut self.0);
    }
}

/// 面板提示（NOTIFICATION）的保存/恢复。
struct NotificationGuard(Option<Notification>);

impl NotificationGuard {
    fn install() -> Self {
        Self(NOTIFICATION.state().write().take())
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        *NOTIFICATION.state().write() = self.0.take();
    }
}

thread_local! {
    /// 删除请求（测试接缝）——见 `intercept_delete_request`。
    static DELETE_REQUESTS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 删除接缝：记录将被删除的 provider 身份并返回 `true`（调用方据此跳过真实删除）。
///
/// 测试构建下 `request_delete_provider` 总能拿到 `true`，因此配置改写、落盘与
/// ACP 推送分支只能由人工/端到端验证覆盖；生产构建不含这段代码（`#[cfg(test)]`）。
pub(super) fn intercept_delete_request(provider_id: &str) -> bool {
    DELETE_REQUESTS.with(|slot| slot.borrow_mut().push(provider_id.to_string()));
    true
}

/// 取走并清空已记录的删除请求（每次断言前先清，避免跨用例残留）。
fn take_delete_requests() -> Vec<String> {
    DELETE_REQUESTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
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
    pub(super) cursor: State<usize>,
    pub(super) mode: State<LoginPanelMode>,
    pub(super) edit_state: State<Option<LoginEditState>>,
    pub(super) edit_focus: State<LoginEditField>,
    pub(super) edit_cursor: State<usize>,
    pub(super) confirm_delete_target: State<Option<String>>,
}

impl MountedPanel {
    /// 走真实事件入口：本帧列表快照与视口起点按组件同一推导。
    fn event(&self, event: Event, area: Rect) -> EventResult {
        let providers = PROVIDER_LIST.state().read().clone();
        let scroll_start =
            scroll_start_for_selected(*self.cursor.read(), providers.len(), VISIBLE_ITEMS);
        super::handle_login_event(
            event,
            &super::LoginEventCtx {
                area: Some(area),
                cursor: self.cursor,
                mode: self.mode,
                edit_state: self.edit_state,
                edit_focus: self.edit_focus,
                edit_cursor: self.edit_cursor,
                confirm_delete_target: self.confirm_delete_target,
                providers,
                scroll_start,
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
fn mount_panel_at(frames: usize, visitor: impl FnMut(MountedPanel) + 'static) -> Buffer {
    let _guard = VisitorGuard;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    ratatui_kit::test_util::render_frames(
        element!(LoginPanel()),
        TEST_AREA.width,
        TEST_AREA.height,
        frames,
    )
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

/// 删除确认门绑定确认时的身份：确认期间列表刷新（每 tick 重派生）不改变待删目标。
///
/// 旧实现只存下标，`delete_provider(*cursor.read())` 会在刷新后删到另一个 provider。
#[test]
#[serial]
fn delete_confirms_the_provider_that_was_frozen() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _note = NotificationGuard::install();
    let _ = take_delete_requests();

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            // Ctrl+D：冻结光标处（下标 0 = alpha）的身份
            let opened = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            assert_eq!(opened, EventResult::Consumed);
            assert_eq!(
                panel.confirm_delete_target.read().clone(),
                Some("alpha".to_string()),
                "进入确认时必须冻结待删除身份"
            );
            // 列表刷新：下标 0 变成 beta
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            tx.send((
                result,
                *panel.mode.read(),
                panel.confirm_delete_target.read().clone(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, mode, target) = rx.try_recv().expect("确认必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Browse, "确认后退出确认门");
    assert_eq!(target, None, "确认门是一次性的，目标随之清空");
    assert_eq!(
        take_delete_requests(),
        vec!["alpha".to_string()],
        "删除必须按确认时冻结的身份发出，而不是刷新后该下标上的 beta"
    );
}

/// 目标已消失（列表刷新 / 他处删除）时不发送请求、不误删其他项，并给出可见提示。
#[test]
#[serial]
fn delete_skips_request_and_hints_when_target_vanished() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _note = NotificationGuard::install();
    let _ = take_delete_requests();

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            let opened = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            assert_eq!(opened, EventResult::Consumed);
            // 目标从列表消失
            *PROVIDER_LIST.state().write() = vec![provider("beta")];
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            tx.send((result, *panel.mode.read()))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, mode) = rx.try_recv().expect("确认必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Browse);
    assert!(
        take_delete_requests().is_empty(),
        "目标已消失时不得发出删除请求（更不得改成删除当前下标上的 beta）"
    );
    let message = NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|note| note.message.clone())
        .expect("目标消失必须给出可见提示");
    assert!(
        message.contains("alpha"),
        "提示必须指明确认时的目标身份，实际：{message}"
    );
}

/// 确认视图显示的是冻结身份，不随列表刷新换成当前下标上的 provider。
#[test]
#[serial]
fn confirm_view_shows_frozen_target_after_refresh() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);

    let mut frame = 0usize;
    let buf = mount_panel_at(4, move |panel| {
        if frame == 0 {
            let _ = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            // 刷新：下标 0 变成 beta，确认门目标必须仍是 alpha
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        frame += 1;
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains("Provider: alpha"),
        "确认视图必须显示冻结的目标身份：\n{text}"
    );
    assert!(
        !text.contains("Provider: beta"),
        "确认视图不得改成刷新后该下标上的 provider：\n{text}"
    );
}

/// 鼠标点击在确认门内等价于 Enter（click as enter），同样按冻结身份删除。
#[test]
#[serial]
fn click_confirms_frozen_target() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _ = take_delete_requests();

    let mut frame = 0usize;
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            let _ = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        if frame == 1 {
            let result = panel.event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 5,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
                TEST_AREA,
            );
            assert_eq!(result, EventResult::Consumed);
        }
        frame += 1;
    });

    assert_eq!(
        take_delete_requests(),
        vec!["alpha".to_string()],
        "点击确认与 Enter 同语义：按冻结身份删除"
    );
}

/// 编辑状态异常（Edit 模式但没有编辑状态）时在渲染前回退：本帧渲染的就是 Browse 视图。
///
/// 旧实现在渲染体内 `*mode.write()`：本帧先渲染出 Internal error，违背 TUI-RENDER-001。
#[test]
#[serial]
fn edit_mode_without_state_renders_browse_in_the_same_frame() {
    let _list = ProviderListGuard::install(vec![provider("alpha")]);

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let buf = mount_panel_at(2, move |panel| {
        if frame == 0 {
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_state.write() = None;
        }
        if frame == 1 {
            tx.send(*panel.mode.read()).expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let mode = rx.try_recv().expect("末帧必须上报状态");
    assert_eq!(mode, LoginPanelMode::Browse, "状态异常必须回退到 Browse");
    let text = buffer_text(&buf);
    assert!(
        text.contains("providers configured"),
        "回退发生在渲染之前，本帧即渲染 Browse 视图：\n{text}"
    );
    assert!(
        !text.contains("Internal error"),
        "渲染段不得改写状态（回退由 effect 承担）：\n{text}"
    );
}
