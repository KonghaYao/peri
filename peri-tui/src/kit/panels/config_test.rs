//! Tests for panels_config

#[cfg(test)]
use super::*;

#[cfg(test)]
use crate::config::{PeriConfig, TuiConfig};

#[test]
fn test_apply_toggle_row_show_diff_flips() {
    let mut cfg = TuiConfig::default();
    assert!(!cfg.diff_enabled);
    assert!(!cfg.diff_enabled);
    cfg.diff_enabled = !cfg.diff_enabled;
    assert!(cfg.diff_enabled);
    cfg.diff_enabled = !cfg.diff_enabled;
    assert!(!cfg.diff_enabled);
}

#[test]
fn test_apply_toggle_row_cache_warn_flips() {
    let mut cfg = PeriConfig::default();
    assert_eq!(cfg.config.show_cache_warning, None);
    let new = apply_toggle_row(&mut cfg, ROW_CACHE_WARN);
    assert_eq!(new, Some(true));
    assert_eq!(cfg.config.show_cache_warning, Some(true));
}

#[test]
fn test_apply_toggle_row_1m_context_handles_none_initial() {
    // 默认 context_1m = false（unwrap_or(false) → false → toggle 为 true）
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".into();
    let alias = cfg.config.active_alias.clone();
    assert_eq!(
        cfg.config.profiles.get(&alias).map(|p| p.context_1m),
        Some(false)
    );
    let new = apply_toggle_row(&mut cfg, ROW_1M_CONTEXT);
    assert_eq!(new, Some(true));
    assert_eq!(
        cfg.config.profiles.get(&alias).map(|p| p.context_1m),
        Some(true)
    );
    let new = apply_toggle_row(&mut cfg, ROW_1M_CONTEXT);
    assert_eq!(new, Some(false));
    assert_eq!(
        cfg.config.profiles.get(&alias).map(|p| p.context_1m),
        Some(false)
    );
}

#[test]
fn test_apply_toggle_row_invalid_returns_none() {
    let mut cfg = PeriConfig::default();
    // ROW_STREAMING 是 Cycle 不是 Toggle——应返回 None
    assert_eq!(apply_toggle_row(&mut cfg, ROW_STREAMING), None);
    // 越界 row
    assert_eq!(apply_toggle_row(&mut cfg, 99), None);
}

#[test]
fn test_apply_cycle_row_streaming_forward_wraps() {
    let mut cfg = TuiConfig {
        streaming_mode: Some("none".into()),
        ..Default::default()
    };
    let next = apply_cycle_row_tui(&mut cfg, ROW_STREAMING, true);
    assert_eq!(next, Some(0)); // wrap to streaming
    assert_eq!(cfg.streaming_mode.as_deref(), Some("streaming"));
}

#[test]
fn test_apply_cycle_row_alias_backward() {
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "fable".into(); // idx=0
    let prev = apply_cycle_row(&mut cfg, ROW_ACTIVE_ALIAS, false);
    assert_eq!(prev, Some(3)); // wrap to haiku
    assert_eq!(cfg.config.active_alias, "haiku");
}

#[test]
fn test_apply_cycle_row_language_forward_from_unknown_resets() {
    // 当前值为非选项时，unwrap_or(0) → 视为 idx=0，forward 后到 idx=1
    let mut cfg = PeriConfig::default();
    cfg.config.language = Some("fr".into()); // 非合法选项
    let next = apply_cycle_row(&mut cfg, ROW_LANGUAGE, true);
    assert_eq!(next, Some(1));
    assert_eq!(cfg.config.language.as_deref(), Some("zh-CN"));
}

#[test]
fn test_apply_cycle_row_invalid_returns_none() {
    let mut cfg = PeriConfig::default();
    // ROW_SHOW_DIFF 是 Toggle 不是 Cycle——应返回 None
    assert_eq!(apply_cycle_row(&mut cfg, ROW_SHOW_DIFF, true), None);
    assert_eq!(apply_cycle_row(&mut cfg, 99, true), None);
}

#[test]
fn test_parse_permission_mode_roundtrip() {
    for opt in PERMISSION_OPTS {
        let mode = parse_permission_mode(opt);
        assert!(mode.is_some(), "{} 应解析成功", opt);
        let label = permission_mode_label(mode.unwrap());
        assert_eq!(label, *opt, "label 与原始字符串应一致");
    }
}

#[test]
fn test_parse_permission_mode_invalid() {
    assert!(parse_permission_mode("invalid").is_none());
    assert!(parse_permission_mode("").is_none());
}

// ── 面板级用例：共享视口、命中/绘制同源、保存失败与句柄缺失 ──────────────────
//
// `State<T>` 无公开构造 API，句柄只能从真实挂载捕获（visitor 模式，同 memory /
// cron 面板测试）；保存入口经 `activate_row_with` 注入，单测绝不写真实配置文件。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{NOTIFICATION, Notification, PANEL_SCROLL_OWNER, PERI_CONFIG_HANDLE};
use crate::kit::panels::shared::list::{ListViewport, content_rows};
use fluent_bundle::FluentValue;
use ratatui_kit::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::{EventResult, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;
use std::cell::RefCell;
use std::rc::Rc;

/// 测试面板区域（渲染尺寸 = 组件区域）：内容区 18 行 = header 2 + 10 行行模型
/// + footer 2，行模型整体落在视口内。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 20,
};

/// 矮面板：内容区 8 行 → 可见 4 行（header 2 + 4 + footer 2）。
const SHORT_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 10,
};

/// beta 区块标题行在行模型中的下标：不可激活（点击只改选中项，不写配置）。
fn beta_section_row() -> usize {
    super::CONFIG_ROWS.len()
}

/// 行模型末行（beta flag 行）。
fn last_row() -> usize {
    super::config_rows().len() - 1
}

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct PanelAtomsGuard {
    notification: Option<Notification>,
    scroll_owner: Option<crate::kit::panel_scroll::PanelScrollOwner>,
}

impl PanelAtomsGuard {
    fn install() -> Self {
        Self {
            notification: std::mem::take(&mut *NOTIFICATION.state().write()),
            scroll_owner: std::mem::take(&mut *PANEL_SCROLL_OWNER.state().write()),
        }
    }
}

impl Drop for PanelAtomsGuard {
    fn drop(&mut self) {
        *NOTIFICATION.state().write() = self.notification.take();
        *PANEL_SCROLL_OWNER.state().write() = self.scroll_owner.take();
    }
}

/// 最近一次通知文本（面板的成功/失败呈现机制）。
fn latest_notification() -> String {
    NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|notification| notification.message.clone())
        .unwrap_or_default()
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

/// 缓冲中首次出现 `needle` 的终端行文本——即鼠标命中的绝对行号。
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

/// 缓冲中首次出现 `needle` 的终端行号。
fn rendered_row_of(buf: &Buffer, needle: &str) -> u16 {
    rendered_line_of(buf, needle).0
}

// ── 挂载探针（`State<T>` 无公开构造 API，句柄只能从真实挂载中捕获）───────────

type MountVisitor = Option<Box<dyn FnMut(MountedPanel)>>;

thread_local! {
    static MOUNT_VISITOR: RefCell<MountVisitor> = const { RefCell::new(None) };
}

pub(super) struct MountedPanel {
    pub(super) cursor: State<usize>,
    pub(super) bump: State<u32>,
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
        element!(super::ConfigPanel()),
        area.width,
        area.height,
        frames,
    )
}

impl MountedPanel {
    /// 走真实事件入口：视口按 `area` 推导，与组件渲染同源。
    fn event(&self, event: Event, area: Rect) -> EventResult {
        let rows = super::config_rows();
        let viewport = ListViewport::for_content_rows(
            content_rows(Some(area), area.height),
            super::LIST_METRICS,
            rows.len(),
            *self.cursor.read(),
        );
        super::handle_config_event(
            event,
            &super::ConfigEventCtx {
                area: Some(area),
                cursor: self.cursor,
                bump: self.bump,
                viewport,
                row_count: rows.len(),
            },
        )
    }
}

/// 当前注册的面板滚动槽位区域（渲染期只有本面板注册）。
fn registered_scroll_area() -> Option<Rect> {
    PANEL_SCROLL_OWNER.state().read().as_ref().map(|owner| {
        assert_eq!(owner.kind, PanelKind::Config);
        owner.slots.first().expect("Config 面板注册单槽位").area
    })
}

// ── 视口：可见行数随面板高度变化 ────────────────────────────────────────────

/// 渲染窗口由面板实测内容区行数推导，不再整表渲染后靠 ScrollView 裁剪。
#[test]
#[serial]
fn rendered_rows_follow_panel_height() {
    let _atoms = PanelAtomsGuard::install();
    let first = i18n::tr("config-field-diff");
    // 行模型第 5 行（下标 4）——矮面板只放得下前 4 行
    let fifth = i18n::tr("config-field-language");

    let short = buffer_text(&mount_panel_frames(SHORT_AREA, 2, |_, _| {}));
    assert!(short.contains(&first), "矮面板至少渲染第一行：{short}");
    assert!(
        !short.contains(&fifth),
        "内容区 8 行只放得下 4 行（2+4+2），不应渲染第 5 行：{short}"
    );

    let tall = buffer_text(&mount_panel_frames(TEST_AREA, 2, |_, _| {}));
    assert!(
        tall.contains(&i18n::tr("config-beta-section")),
        "面板变高后必须渲染 beta 区块，而不是停在矮面板的 4 行：{tall}"
    );
}

// ── 命中与绘制同源 ─────────────────────────────────────────────────────────

/// 点击渲染出的行 → 该行成为高亮项（下一帧渲染可见）。
#[test]
#[serial]
fn click_on_rendered_row_highlights_that_row() {
    let _atoms = PanelAtomsGuard::install();
    let section = i18n::tr("config-beta-section");

    let buf = mount_panel_frames(TEST_AREA, 2, |_, _| {});
    let row = rendered_row_of(&buf, &section);

    let buf = mount_panel_frames(TEST_AREA, 2, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(left_down(row, 5), TEST_AREA);
            assert_eq!(result, EventResult::Consumed);
            assert_eq!(
                *panel.cursor.read(),
                beta_section_row(),
                "点击必须选中被点的那一行"
            );
        }
    });
    let (_, line) = rendered_line_of(&buf, &section);
    assert!(
        line.trim_start().starts_with('>'),
        "被点中的行必须在下一帧渲染为高亮项，实际行：{line:?}"
    );
}

/// 矮面板：窗口跟随选中项滚动，命中取同一窗口——点击「渲染出的那一行」选中的
/// 是同一行（旧实现 `visible_items = 整表行数, scroll_start = 0` 会指到窗口外的行）。
#[test]
#[serial]
fn short_panel_hit_matches_scrolled_window() {
    let _atoms = PanelAtomsGuard::install();
    let section = i18n::tr("config-beta-section");
    let last = last_row();

    let buf = mount_panel_frames(SHORT_AREA, 2, move |_, panel| {
        *panel.cursor.write() = last;
    });
    assert!(
        buffer_text(&buf).contains(&section),
        "选中末行时窗口必须滚到末尾 4 行（含 beta 区块）：\n{}",
        buffer_text(&buf)
    );
    let row = rendered_row_of(&buf, &section);
    assert!(
        row < SHORT_AREA.height,
        "区块标题行必须落在面板区域内（实际行 {row}）"
    );

    mount_panel_frames(SHORT_AREA, 2, move |frame, panel| {
        *panel.cursor.write() = last;
        if frame == 0 {
            let result = panel.event(left_down(row, 5), SHORT_AREA);
            assert_eq!(result, EventResult::Consumed);
            assert_eq!(
                *panel.cursor.read(),
                beta_section_row(),
                "点击窗口内的行必须选中同一行"
            );
        }
    });
}

/// 滚动仲裁注册区域与鼠标命中共用 `AreaTracker`：首帧未测量时 0 尺寸，
/// 之后为实测面板区域（此前用 `use_previous_size()`，与命中区域不同源）。
#[test]
#[serial]
fn scroll_area_comes_from_measured_panel_rect() {
    let _atoms = PanelAtomsGuard::install();

    mount_panel_frames(TEST_AREA, 2, |frame, _panel| {
        if frame == 1 {
            // 帧 0 组件尚未测量自身区域，注册 0 尺寸而不是猜测的尺寸
            assert_eq!(registered_scroll_area(), Some(Rect::default()));
        }
    });

    assert_eq!(
        registered_scroll_area(),
        Some(TEST_AREA),
        "注册区域必须是 AreaTracker 的实测区域（与鼠标命中同源）"
    );
}

// ── 保存失败与句柄缺失：如实可见、不改内存 ──────────────────────────────────

/// 保存成功：保存入口收到写入后的候选快照，成功后才提交句柄。
#[test]
#[serial]
fn base_row_save_success_commits_candidate() {
    let _atoms = PanelAtomsGuard::install();
    let handle = super::beta_tests::ensure_config_handle();
    handle.write().config.show_cache_warning = Some(false);

    let seen = Rc::new(RefCell::new(None));
    let captured = Rc::clone(&seen);
    super::activate_row_with(
        PERI_CONFIG_HANDLE.get(),
        super::ROW_CACHE_WARN,
        true,
        move |candidate| {
            *captured.borrow_mut() = candidate.config.show_cache_warning;
            Ok(())
        },
    );

    assert_eq!(
        *seen.borrow(),
        Some(true),
        "保存入口必须收到写入了新值的候选快照"
    );
    assert_eq!(
        handle.read().config.show_cache_warning,
        Some(true),
        "落盘成功后句柄才显示新值"
    );
    handle.write().config.show_cache_warning = None;
}

/// 保存失败：提示携带失败原因，句柄保持已持久化的值（不得显示未落盘的开关）。
#[test]
#[serial]
fn base_row_save_failure_keeps_persisted_value_and_reports() {
    let _atoms = PanelAtomsGuard::install();
    let handle = super::beta_tests::ensure_config_handle();
    handle.write().config.show_cache_warning = Some(false);

    super::activate_row_with(
        PERI_CONFIG_HANDLE.get(),
        super::ROW_CACHE_WARN,
        true,
        |_| Err(anyhow::anyhow!("configuration revision conflict")),
    );

    assert_eq!(
        handle.read().config.show_cache_warning,
        Some(false),
        "保存失败不得留下未落盘的开关"
    );
    let notification = latest_notification();
    let expected = i18n::tr_args(
        "config-save-failed",
        &[(
            "error".to_string(),
            FluentValue::from("configuration revision conflict"),
        )],
    );
    assert_eq!(
        notification, expected,
        "失败提示必须是保存失败模板并携带失败原因"
    );
    handle.write().config.show_cache_warning = None;
}

/// 句柄缺失：动作不执行、不调用保存入口，并给出可见失败提示（此前静默 return）。
#[test]
#[serial]
fn missing_config_handle_reports_failure_and_skips_write() {
    let _atoms = PanelAtomsGuard::install();
    let handle = super::beta_tests::ensure_config_handle();
    let before = handle.read().clone();

    let saved = Rc::new(RefCell::new(false));
    let captured = Rc::clone(&saved);
    super::activate_row_with(None, super::ROW_CACHE_WARN, true, move |_| {
        *captured.borrow_mut() = true;
        Ok(())
    });

    assert!(
        !*saved.borrow(),
        "句柄缺失不得调用保存入口（不产生错误写入）"
    );
    assert_eq!(*handle.read(), before, "句柄缺失不得改动任何配置字段");
    let notification = latest_notification();
    assert!(
        notification.contains("PERI_CONFIG_HANDLE"),
        "失败提示必须指名缺失的句柄：{notification}"
    );
}

/// 活动档位不存在（`ROW_1M_CONTEXT`）：没有字段可写时如实报告失败，
/// 不再走保存并提示「已保存」。
#[test]
#[serial]
fn missing_active_profile_reports_failure() {
    let _atoms = PanelAtomsGuard::install();
    let handle = super::beta_tests::ensure_config_handle();
    let previous_alias = handle.read().config.active_alias.clone();
    let previous_flag = handle
        .read()
        .config
        .profiles
        .get(&previous_alias)
        .map(|profile| profile.context_1m);
    handle.write().config.active_alias = "definitely-missing-profile".into();

    let saved = Rc::new(RefCell::new(false));
    let captured = Rc::clone(&saved);
    super::activate_row_with(
        PERI_CONFIG_HANDLE.get(),
        super::ROW_1M_CONTEXT,
        true,
        move |_| {
            *captured.borrow_mut() = true;
            Ok(())
        },
    );
    handle.write().config.active_alias = previous_alias;

    assert!(!*saved.borrow(), "无字段可写时不得落盘");
    assert!(
        latest_notification().contains("definitely-missing-profile"),
        "失败提示必须说明是哪个档位缺失：{}",
        latest_notification()
    );
    if let Some(flag) = previous_flag {
        let alias = handle.read().config.active_alias.clone();
        if let Some(profile) = handle.write().config.profiles.get_mut(&alias) {
            profile.context_1m = flag;
        }
    }
}

/// 权限模式行改的是当前会话运行权限（`session/set_mode`），不写宿主持久配置：
/// 不得调用保存入口（单测进程无 ACP client，`permission_mode::request` 为空操作）。
#[test]
#[serial]
fn permission_mode_row_never_persists() {
    let _atoms = PanelAtomsGuard::install();
    let handle = super::beta_tests::ensure_config_handle();
    let before = handle.read().clone();

    let saved = Rc::new(RefCell::new(false));
    let captured = Rc::clone(&saved);
    super::activate_row_with(
        PERI_CONFIG_HANDLE.get(),
        super::ROW_PERMISSION_MODE,
        true,
        move |_| {
            *captured.borrow_mut() = true;
            Ok(())
        },
    );

    assert!(!*saved.borrow(), "会话权限不得走宿主配置保存入口");
    assert_eq!(*handle.read(), before, "会话权限不得改写宿主配置");
}

/// beta flag 行经同一保存边界：句柄缺失时同样「不改内存 + 可见失败提示」。
#[test]
#[serial]
fn beta_flag_without_handle_reports_failure() {
    let _atoms = PanelAtomsGuard::install();
    let handle = super::beta_tests::ensure_config_handle();
    let before = handle.read().clone();

    super::toggle_beta_flag_with(None, peri_acp_types::beta_flags::FULL_ASYNC_TOOLS, |_| {
        Ok(())
    });

    assert_eq!(*handle.read(), before, "句柄缺失不得改动 beta 覆盖");
    assert!(
        latest_notification().contains("PERI_CONFIG_HANDLE"),
        "失败提示必须指名缺失的句柄：{}",
        latest_notification()
    );
}

// ── hook 顺序（TUI-HOOK-001） ──────────────────────────────────────────────

/// 跨帧挂载：不同终端高度/选中项下 hook 数量与类型不变——ratatui-kit 的
/// `use_hook` 按调用下标取已注册 hook 并做类型校验，顺序偏移会直接 panic。
#[test]
#[serial]
fn hooks_stay_stable_across_frames() {
    let _atoms = PanelAtomsGuard::install();
    for area in [TEST_AREA, SHORT_AREA, Rect::new(0, 0, 80, 40)] {
        let buf = mount_panel_frames(area, 3, |frame, panel| {
            // 每帧换一个选中项：窗口起点与渲染分支随之变化
            *panel.cursor.write() = frame * 3;
        });
        assert!(
            buffer_text(&buf).contains(&i18n::tr("config-panel-title")),
            "每个尺寸/选中态都必须渲染出面板标题"
        );
    }
}

/// TUI-HOOK-001（静态面）：组件体内所有 `hooks.use_*` 都在事件注册之前，且事件
/// 注册之前没有任何提前 `return`——hook 数量与顺序不可能随渲染路径变化。
#[test]
fn config_hooks_precede_event_registration_and_returns() {
    let src = include_str!("config.rs");
    let body = src
        .split("#[path = \"config_test.rs\"]")
        .next()
        .expect("组件模块体");
    let component = body.split("pub fn ConfigPanel").nth(1).expect("组件函数体");
    let handler = component
        .find("hooks.use_event_handler_with_options")
        .expect("事件注册存在");

    assert!(
        component[..handler].contains("hooks.use_state")
            && component[..handler].contains("hooks.use_atom")
            && component[..handler].contains("hooks.use_hook"),
        "状态/atom/区域 hook 都必须在事件注册之前无条件调用"
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
