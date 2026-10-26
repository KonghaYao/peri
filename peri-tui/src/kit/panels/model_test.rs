use super::edit::edit_field;
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
use ratatui_kit::components::scroll_view::ScrollViewState;
use ratatui_kit::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
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
    config: crate::config::PeriConfig,
    notification: Option<Notification>,
    scroll_owner: Option<crate::kit::panel_scroll::PanelScrollOwner>,
    open_panels: Vec<PanelKind>,
    active_panel: Option<PanelKind>,
    login_request: Option<crate::kit::atoms::LoginPanelRequest>,
}

impl ModelPanelGuard {
    fn install() -> Self {
        let handle = ensure_config_handle();
        let config = handle.read().clone();
        let notification = std::mem::take(&mut *NOTIFICATION.state().write());
        let scroll_owner = std::mem::take(&mut *PANEL_SCROLL_OWNER.state().write());
        let open_panels = std::mem::take(&mut *crate::kit::atoms::OPEN_PANELS.state().write());
        let active_panel = std::mem::take(&mut *crate::kit::atoms::ACTIVE_PANEL.state().write());
        let login_request =
            std::mem::take(&mut *crate::kit::atoms::LOGIN_PANEL_REQUEST.state().write());
        Self {
            handle,
            config,
            notification,
            scroll_owner,
            open_panels,
            active_panel,
            login_request,
        }
    }

    fn set_active_alias(&self, alias: &str) {
        self.handle.write().config.active_alias = alias.to_string();
    }

    fn set_config(&self, config: crate::config::PeriConfig) {
        *self.handle.write() = config;
    }
}

impl Drop for ModelPanelGuard {
    fn drop(&mut self) {
        *self.handle.write() = self.config.clone();
        *NOTIFICATION.state().write() = self.notification.take();
        *PANEL_SCROLL_OWNER.state().write() = self.scroll_owner.take();
        *crate::kit::atoms::OPEN_PANELS.state().write() = std::mem::take(&mut self.open_panels);
        *crate::kit::atoms::ACTIVE_PANEL.state().write() = self.active_panel.take();
        *crate::kit::atoms::LOGIN_PANEL_REQUEST.state().write() = self.login_request.take();
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
    pub(super) window_edit: State<Option<String>>,
    pub(super) render_version: State<u64>,
    pub(super) right_scroll: State<ScrollViewState>,
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
        super::panel_input::handle_model_event_with(
            event,
            &super::panel_input::ModelEventCtx {
                area: Some(area),
                right_cursor: self.right_cursor,
                right_focus: self.right_focus,
                window_edit: self.window_edit,
                render_version: self.render_version,
                right_scroll: self.right_scroll,
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

// ── Responses contextWindow 内联编辑 ───────────────────────────────────────

fn key(code: KeyCode) -> Event {
    key_with(code, KeyModifiers::NONE)
}

fn key_with(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn responses_config() -> crate::config::PeriConfig {
    serde_json::from_value(serde_json::json!({"config": {
        "active_alias": "sonnet",
        "providers": [
            {"id":"resp", "type":"openai_responses", "apiKey":"fixture-key",
             "models":{"fable":"fable-model", "opus":"opus-model", "sonnet":"sonnet-model", "haiku":"haiku-model"}},
            {"id":"other", "type":"openai", "apiKey":"other-key",
             "models":{"fable":"other-fable", "opus":"other-opus", "sonnet":"other-sonnet", "haiku":"other-haiku"}}
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

#[path = "model/hit_test.rs"]
mod hit;

#[path = "model/responses_test.rs"]
mod responses;

#[path = "model/mouse_test.rs"]
mod mouse;

#[path = "model/provider_test.rs"]
mod provider;
