//! Tests for LoginPanel component.
//!
//! 面板级用例走真实事件入口：`State<T>` 无公开构造 API，句柄从挂载探针捕获
//! （visitor 模式，同 memory/mcp 面板测试）。删除请求经
//! `intercept_delete_request` 接缝记录——测试进程的 `PERI_CONFIG_HANDLE` 是
//! 不可安装的进程级 `OnceLock`，真实删除路径（配置改写 + 落盘 + ACP 推送）不在
//! 单测覆盖范围内。

use super::*;

#[path = "login/commit_test.rs"]
mod commit;

use super::config_store::delete_candidate;
use super::edit_state::ProviderKind;
use crate::config::ProviderConfig;
use crate::kit::atoms::PROVIDER_LIST;
use ratatui_kit::crossterm::event::{Event, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::{Position, Rect};
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
        has_chatgpt_plan: false,
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
    pub(super) edit_focus: State<LoginField>,
    pub(super) edit_cursor: State<usize>,
    pub(super) confirm_delete_target: State<Option<String>>,
    pub(super) auth: State<super::auth::AuthController>,
    pub(super) sv: State<ratatui_kit::components::scroll_view::ScrollViewState>,
}

impl MountedPanel {
    /// 走真实事件入口：本帧列表快照与视口按组件同一推导。
    fn event(&self, event: Event, area: Rect) -> EventResult {
        let providers = PROVIDER_LIST.state().read().clone();
        let viewport = super::browse_render::viewport(
            super::content_rows(Some(area), 0),
            providers.len(),
            *self.cursor.read(),
        );
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
                auth: self.auth,
                sv: self.sv,
                providers,
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
/// 测试里组件直接铺满整个终端缓冲，因此这里的 `height` 是组件高度（= 终端高度），
/// 而不是 `panel_overlay::panel_height` 的预测值；后者只用于首帧内容行数预测。
fn mount_panel_sized(
    frames: usize,
    width: u16,
    height: u16,
    visitor: impl FnMut(MountedPanel) + 'static,
) -> Buffer {
    let _guard = VisitorGuard;
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none(), "上一个 visitor 未清理");
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    ratatui_kit::test_util::render_frames(element!(LoginPanel()), width, height, frames)
}

fn mount_panel_at(frames: usize, visitor: impl FnMut(MountedPanel) + 'static) -> Buffer {
    mount_panel_sized(frames, TEST_AREA.width, TEST_AREA.height, visitor)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn buffer_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        out.push_str(&buffer_line(buf, y));
        out.push('\n');
    }
    out
}

/// 单行可见文本（用于断言提示行位置与整行内容）。
fn buffer_line(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
}

fn login_config() -> crate::config::PeriConfig {
    serde_json::from_value(serde_json::json!({"config": {
        "active_alias": "sonnet",
        "providers": [
            {"id":"resp", "type":"openai_responses", "apiKey":"fixture-key",
             "models":{"fable":"fable-model", "opus":"opus-model", "sonnet":"sonnet-model", "haiku":"haiku-model"}},
            {"id":"plan", "type":"openai_responses", "apiKey":"",
             "auth":{"kind":"chatgpt_plan", "store":"/tmp/peri-login-test", "account":"work",
                     "subject":"subject", "clientId":"client"},
             "models":{"fable":"plan-fable", "opus":"plan-opus", "sonnet":"plan-sonnet", "haiku":"plan-haiku"}}
        ],
        "profiles": {
            "fable":{"provider":"resp", "contextWindow":1000},
            "opus":{"provider":"resp", "contextWindow":2000},
            "sonnet":{"provider":"resp", "contextWindow":3000},
            "haiku":{"provider":"resp", "contextWindow":4000}
        }
    }}))
    .unwrap()
}

fn mount_edit_state(
    state: LoginEditState,
    focus: LoginField,
    frames: usize,
    mut visitor: impl FnMut(usize, MountedPanel) + 'static,
) -> Buffer {
    let mut frame = 0usize;
    mount_panel_at(frames, move |panel| {
        if frame == 0 {
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_state.write() = Some(state.clone());
            *panel.edit_focus.write() = focus;
        }
        visitor(frame, panel);
        frame += 1;
    })
}

/// 指定宽高挂载 Edit 视图（复用同一 `mount_panel_sized` 管道）。
fn mount_edit_state_sized(
    state: LoginEditState,
    focus: LoginField,
    frames: usize,
    width: u16,
    height: u16,
    mut visitor: impl FnMut(usize, MountedPanel) + 'static,
) -> Buffer {
    let mut frame = 0usize;
    mount_panel_sized(frames, width, height, move |panel| {
        if frame == 0 {
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_state.write() = Some(state.clone());
            *panel.edit_focus.write() = focus;
        }
        visitor(frame, panel);
        frame += 1;
    })
}

// ── 鼠标命中：Edit 动态字段 / choice / 类型确认 ─────────────────────────────

fn test_render_colors() -> super::render::LoginRenderColors {
    super::render::LoginRenderColors::reset()
}

fn edit_layout(panel: &MountedPanel, width: u16) -> super::render::EditLayout {
    let state = panel
        .edit_state
        .read()
        .clone()
        .expect("Edit 模式必须有草稿");
    let auth = panel.auth.read();
    super::render::build_edit_layout(
        &state,
        &auth,
        *panel.edit_focus.read(),
        *panel.edit_cursor.read(),
        &test_render_colors(),
        width,
    )
}

fn hit_semantic_row(panel: &MountedPanel, hit: super::render::EditHit, width: u16) -> usize {
    edit_layout(panel, width)
        .semantic_row(hit)
        .unwrap_or_else(|| panic!("命中区必须包含目标"))
}

/// 单行动作区里某动作的 (语义行, 起始内容列)。
fn action_hit_span(
    panel: &MountedPanel,
    action: super::auth::AuthAction,
    width: u16,
) -> (usize, usize) {
    let layout = edit_layout(panel, width);
    let span = layout
        .action_spans()
        .iter()
        .find(|span| span.action == action)
        .unwrap_or_else(|| panic!("动作区必须包含目标 {action:?}"));
    (span.row, span.start)
}

fn edit_hit_row(panel: &MountedPanel, hit: super::render::EditHit, area: Rect) -> u16 {
    let offset = panel.sv.read().offset().y;
    area.y + 1 + hit_semantic_row(panel, hit, area.width) as u16 - offset
}

fn type_change_semantic_row(panel: &MountedPanel, hit: super::render::EditHit) -> usize {
    let state = panel
        .edit_state
        .read()
        .clone()
        .expect("类型确认视图必须有草稿");
    super::render::build_type_change_layout(&state, &test_render_colors())
        .hits
        .iter()
        .position(|candidate| *candidate == Some(hit))
        .unwrap_or_else(|| panic!("类型确认命中区必须包含目标"))
}

fn tall_area() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 60,
    }
}

fn click_at(row: u16, column: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn new_responses_mouse_state() -> LoginEditState {
    let cfg = login_config();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.kind = Some(ProviderKind::OpenAiResponses);
    state.raw_type = ProviderKind::OpenAiResponses.as_str().to_string();
    state.provider_id = "new".into();
    state.api_key = "key".into();
    state.models.fable = "new-fable".into();
    state.models.opus = "new-opus".into();
    state.models.sonnet = "new-sonnet".into();
    state.models.haiku = "new-haiku".into();
    state
}

#[path = "login/browse_test.rs"]
mod browse;

#[path = "login/visual_test.rs"]
mod visual;

#[path = "login/form_test.rs"]
mod form;

#[path = "login/mouse_test.rs"]
mod mouse;

#[path = "login/auth_ui_test.rs"]
mod auth_ui;

#[path = "login/flow_test.rs"]
mod flow;
