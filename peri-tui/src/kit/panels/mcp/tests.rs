//! MCP 面板测试：详情布局契约、弹窗让行、动态视口与稳定身份。

use super::detail::{
    DETAIL_HEADER_ROW, DETAIL_RIGHT_RESERVE, DETAIL_WRAP_WIDTH, DetailAction, DetailButton,
    SkillEntry, build_detail_lines, button_style, cache_status_label_key, default_detail_btn,
    detail_action_at, detail_actions, detail_btn_row, detail_buttons, detail_error_lines,
    format_connected_at,
};
use super::{McpView, detail_copy_text};
use crate::i18n;
use crate::kit::atoms::McpServerSummary;
use crate::kit::text_util::wrap_text;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, MCP_SERVERS, OAUTH_INFO, POPUP_KIND, PopupKind};
use crate::kit::panels::shared::list::{ListViewport, content_rows};
use peri_acp_types::event_data::OauthNeeded;
use ratatui_kit::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui_kit::prelude::{EventResult, ScrollViewState, State, element};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::{Position, Rect};
use serial_test::serial;

/// 测试用面板内容宽度（82 列 + 滚动条预留 → 默认面板宽度量级）。
pub(super) const TEST_WIDTH: u16 = 60;

pub(super) fn detail_lines(
    summary: &McpServerSummary,
    btn_sel: usize,
) -> Vec<ratatui::text::Line<'static>> {
    detail_lines_with_skills(summary, &[], btn_sel)
}

pub(super) fn detail_lines_with_skills(
    summary: &McpServerSummary,
    skills: &[SkillEntry],
    btn_sel: usize,
) -> Vec<ratatui::text::Line<'static>> {
    build_detail_lines(
        summary,
        skills,
        btn_sel,
        &peri_theme::builtin::dark_theme(),
        TEST_WIDTH,
    )
}

pub(super) fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// 渲染出的行下标（needle 为按钮渲染文本——复制按钮无方括号、底行按钮带方括号）。
pub(super) fn rendered_row(lines: &[ratatui::text::Line<'_>], needle: &str) -> usize {
    lines
        .iter()
        .position(|line| line_text(line).contains(needle))
        .unwrap_or_else(|| panic!("[{needle}] 必须渲染"))
}

fn copy_button(summary: &McpServerSummary, width: u16) -> DetailButton {
    detail_buttons(summary, 0, width)
        .into_iter()
        .find(|button| button.action == DetailAction::CopyError)
        .expect("有错误摘要必须布局复制按钮")
}

/// 行内指定内容的 span（渲染样式断言用）。
fn span_with<'a>(line: &'a ratatui::text::Line<'a>, text: &str) -> &'a ratatui::text::Span<'a> {
    line.spans
        .iter()
        .find(|span| span.content.as_ref() == text)
        .unwrap_or_else(|| panic!("span {text} 必须渲染"))
}

#[test]
fn detail_button_row_counts_empty_url_value_line() {
    assert_eq!(
        detail_btn_row(&McpServerSummary::default(), 0, TEST_WIDTH),
        21
    );
}

/// `mcp/oauth_start` 的 Ok 响应失败判定：仅 `success=false`（conflict）触发
/// 提示；成功、缺失或非布尔 success 不提示；status 缺失时保底 "conflict"。
#[test]
fn oauth_start_failure_message_detects_conflict() {
    assert_eq!(
        super::oauth_start_failure_message(
            &serde_json::json!({"success": false, "status": "conflict"})
        )
        .as_deref(),
        Some("conflict")
    );
    assert_eq!(
        super::oauth_start_failure_message(&serde_json::json!({"success": false})).as_deref(),
        Some("conflict")
    );
    assert_eq!(
        super::oauth_start_failure_message(
            &serde_json::json!({"success": true, "status": "started"})
        ),
        None
    );
    assert_eq!(
        super::oauth_start_failure_message(&serde_json::json!({})),
        None
    );
    assert_eq!(
        super::oauth_start_failure_message(&serde_json::json!({"success": "yes"})),
        None
    );
}

/// 鼠标命中的按钮行号必须与真实渲染出的按钮行一致：常量与渲染漂移时
/// 点击会落到错误行。覆盖长错误（多行换行）、长 URL 与待授权的组合。
#[test]
fn detail_button_row_matches_rendered_button_line() {
    let cases = [
        McpServerSummary::default(),
        McpServerSummary {
            error_summary: Some("boom".into()),
            ..McpServerSummary::default()
        },
        McpServerSummary {
            error_summary: Some("E".repeat(200)),
            ..McpServerSummary::default()
        },
        McpServerSummary {
            url: Some(format!("https://example.com/{}", "p".repeat(120))),
            ..McpServerSummary::default()
        },
        McpServerSummary {
            error_summary: Some("E".repeat(120)),
            url: Some("https://example.com/mcp/sse".into()),
            needs_auth: true,
            ..McpServerSummary::default()
        },
    ];
    for summary in cases {
        let lines = detail_lines(&summary, 0);
        let back = DetailAction::Back.text(&i18n::tr("panel-mcp-detail-btn-back"));
        let rendered = rendered_row(&lines, &back);
        assert_eq!(rendered, detail_btn_row(&summary, 0, TEST_WIDTH) as usize);
    }
}

/// 长错误按 DETAIL_WRAP_WIDTH 换行且内容完整（不丢字符、不截断）。
#[test]
fn long_error_summary_wraps_and_stays_complete() {
    let error = format!(
        "connection refused: 127.0.0.1:8080 unreachable{}",
        " (retry) ".repeat(4)
    );
    let summary = McpServerSummary {
        error_summary: Some(error.clone()),
        ..McpServerSummary::default()
    };
    let wrapped = wrap_text(&error, DETAIL_WRAP_WIDTH);
    assert!(wrapped.len() > 1, "用例必须覆盖多行换行");

    let lines = detail_lines(&summary, 0);
    let label = i18n::tr("panel-mcp-detail-error");
    let start = lines
        .iter()
        .position(|line| line_text(line).contains(&label))
        .expect("错误标题必须渲染");
    let rendered: Vec<String> = lines[start + 1..start + 1 + wrapped.len()]
        .iter()
        .map(line_text)
        .collect();
    assert_eq!(
        rendered,
        wrapped
            .iter()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
    );
    assert!(
        rendered
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= DETAIL_WRAP_WIDTH + 4),
        "换行后每行不得超过缩进 + 换行宽度"
    );
    // 完整内容：去掉缩进后拼回原文
    assert_eq!(
        rendered
            .iter()
            .map(|line| line.trim_start())
            .collect::<String>(),
        error
    );
    assert_eq!(detail_error_lines(&summary), Some(wrapped.len()));
}

/// 复制按钮与标题同行（header 行）渲染，仅在错误摘要存在时出现。
#[test]
fn header_copy_button_shares_title_row_and_hides_without_error() {
    let copy_label = i18n::tr("panel-mcp-detail-btn-copy");
    let copy_text = DetailAction::CopyError.text(&copy_label);
    let with_error = McpServerSummary {
        error_summary: Some("boom".into()),
        ..McpServerSummary::default()
    };
    let lines = detail_lines(&with_error, 1);
    assert_eq!(
        rendered_row(&lines, &copy_text),
        DETAIL_HEADER_ROW as usize,
        "复制按钮必须与标题同行"
    );
    // 同行而非独占行：复制文本只出现在 header 行
    assert_eq!(
        lines
            .iter()
            .filter(|line| line_text(line).contains(&copy_text))
            .count(),
        1
    );
    // 无错误摘要：不渲染复制按钮（无可复制内容，且保持既有布局不变）
    let lines = detail_lines(&McpServerSummary::default(), 0);
    assert!(
        !lines
            .iter()
            .any(|line| line_text(line).contains(&copy_text)),
        "无错误摘要时不应渲染复制按钮"
    );
}

/// 复制按钮右对齐到内容右缘（预留滚动条列）；窄终端下标题截断但按钮仍渲染、
/// 命中区间仍落在面板内。
#[test]
fn header_copy_button_is_right_aligned_and_survives_narrow_width() {
    let summary = McpServerSummary {
        name: "very-long-server-name-for-narrow-terminal".into(),
        version: Some("1.2.3".into()),
        error_summary: Some("boom".into()),
        needs_auth: true,
        ..McpServerSummary::default()
    };
    let copy_text = DetailAction::CopyError.text(&i18n::tr("panel-mcp-detail-btn-copy"));
    for width in [80u16, 60, 40, 24, 12] {
        let lines = build_detail_lines(
            &summary,
            &[],
            1, // 默认落在 [ 返回 ]：复制按钮为未选中态
            &peri_theme::builtin::dark_theme(),
            width,
        );
        let button = copy_button(&summary, width);
        let header = &lines[DETAIL_HEADER_ROW as usize];
        let rendered = line_text(header);
        assert!(
            rendered.contains(&copy_text),
            "width={width} 时复制按钮必须仍渲染"
        );
        // 右对齐：按钮左列 = 宽度 − 滚动条预留 − 按钮宽（极窄时贴左列）
        assert_eq!(
            button.col,
            width.saturating_sub(DETAIL_RIGHT_RESERVE + button.width()),
            "width={width} 右对齐列偏移"
        );
        assert_eq!(
            UnicodeWidthStr::width(rendered.as_str()) as u16,
            button.col + button.width(),
            "width={width} 渲染行宽必须与按钮右缘一致"
        );
        // 命中区间落在面板内
        assert!(
            button.col + button.hit_width <= width,
            "width={width} 命中区间不得越出面板"
        );
        // 极窄：标题被截断（省略号），不与按钮重叠
        if width < 40 {
            assert!(rendered.contains('…'), "width={width} 时标题必须被截断让位");
        }
    }
}

/// 按钮样式复用 chat 消息区按钮：复制 = md 复制按钮反色块；底行 = 选项/权限
/// 按钮（选中态 bg(selection)+BOLD）。
#[test]
fn detail_button_styles_match_chat_buttons() {
    let theme = peri_theme::builtin::dark_theme();
    let semantic = &theme.semantic;
    // message_area/render.rs::copy_button_line
    let copy_default = Style::default()
        .fg(semantic.accent)
        .add_modifier(Modifier::REVERSED);
    assert_eq!(
        button_style(DetailAction::CopyError, false, &theme),
        copy_default
    );
    assert_eq!(
        button_style(DetailAction::CopyError, true, &theme),
        copy_default.add_modifier(Modifier::BOLD)
    );
    // message_area/render/interaction.rs 选项按钮 + message_area/mod.rs:719 选中态
    let plain = Style::default().fg(semantic.text.primary);
    assert_eq!(button_style(DetailAction::Back, false, &theme), plain);
    assert_eq!(
        button_style(DetailAction::Auth, true, &theme),
        plain
            .bg(semantic.surface.selection)
            .add_modifier(Modifier::BOLD)
    );

    // 渲染结果与上述样式一致：btn_sel = 1（默认 [ 返回 ]）→ 复制按钮未选中
    let summary = McpServerSummary {
        error_summary: Some("boom".into()),
        ..McpServerSummary::default()
    };
    let copy_text = DetailAction::CopyError.text(&i18n::tr("panel-mcp-detail-btn-copy"));
    let lines = detail_lines(&summary, 1);
    assert_eq!(
        span_with(&lines[DETAIL_HEADER_ROW as usize], &copy_text).style,
        copy_default
    );
    let lines = detail_lines(&summary, 0);
    assert_eq!(
        span_with(&lines[DETAIL_HEADER_ROW as usize], &copy_text).style,
        copy_default.add_modifier(Modifier::BOLD)
    );
}

/// 按钮列偏移必须与渲染位置一致（鼠标按列命中的依据；CJK label 双宽）。
#[test]
fn detail_button_columns_match_rendered_lines() {
    let summary = McpServerSummary {
        error_summary: Some("boom".into()),
        url: Some("https://example.com/mcp".into()),
        needs_auth: true,
        ..McpServerSummary::default()
    };
    let lines = detail_lines(&summary, 0);
    for button in detail_buttons(&summary, 0, TEST_WIDTH) {
        let line = lines
            .get(button.row as usize)
            .expect("按钮行必须在渲染范围内");
        let mut col = 0u16;
        let mut rendered = None;
        for span in &line.spans {
            if span.content.as_ref() == button.text {
                rendered = Some((col, UnicodeWidthStr::width(span.content.as_ref()) as u16));
                break;
            }
            col += UnicodeWidthStr::width(span.content.as_ref()) as u16;
        }
        let (rendered_col, rendered_width) =
            rendered.unwrap_or_else(|| panic!("按钮 {} 必须渲染在其契约行", button.text));
        assert_eq!(rendered_col, button.col, "列偏移漂移：{}", button.text);
        assert_eq!(rendered_width, button.width(), "列宽漂移：{}", button.text);
    }
}

/// 选择索引空间与默认选中：默认显式落在 [ 返回 ]，避免 Enter 误触授权/复制。
#[test]
fn detail_actions_order_and_default_selection() {
    assert_eq!(
        detail_actions(&McpServerSummary::default()),
        vec![DetailAction::Back]
    );
    assert_eq!(default_detail_btn(&McpServerSummary::default()), 0);

    let summary = McpServerSummary {
        error_summary: Some("boom".into()),
        needs_auth: true,
        ..McpServerSummary::default()
    };
    assert_eq!(
        detail_actions(&summary),
        vec![
            DetailAction::CopyError,
            DetailAction::Auth,
            DetailAction::Back
        ]
    );
    assert_eq!(
        detail_action_at(&summary, default_detail_btn(&summary)),
        Some(DetailAction::Back)
    );
    assert_eq!(
        detail_actions(&summary).len(),
        detail_buttons(&summary, 0, TEST_WIDTH).len(),
        "键盘选择索引空间必须与渲染按钮一一对应"
    );
    assert_eq!(detail_action_at(&summary, 3), None);
}

/// 复制按钮相关 key 双语齐备（缺 key 时 tr 回落为 key 原文）。
#[test]
fn copy_button_keys_resolve_in_both_languages() {
    let keys = [
        "panel-mcp-detail-btn-copy",
        "panel-mcp-detail-copied",
        "panel-mcp-detail-copy-failed",
    ];
    for lang in ["en", "zh-CN"] {
        let lc = crate::i18n::LcRegistry::new(Some(lang));
        for key in keys {
            assert_ne!(lc.tr(key), key, "{lang} 缺 key {key}");
        }
    }
}

/// 复制载荷是错误原文（含换行、不截断），与渲染换行无关。
#[test]
fn detail_copy_text_keeps_raw_error_summary() {
    let raw = format!("line1\nline2 {}", "x".repeat(80));
    let summary = McpServerSummary {
        error_summary: Some(raw.clone()),
        ..McpServerSummary::default()
    };
    assert_eq!(detail_copy_text(&summary), Some(raw.as_str()));
    assert_eq!(detail_copy_text(&McpServerSummary::default()), None);
}

#[test]
fn detail_lines_render_protocol_version() {
    let summary = McpServerSummary {
        name: "docs".into(),
        protocol_version: Some("2026-07-28".into()),
        ..McpServerSummary::default()
    };
    let label = i18n::tr("panel-mcp-detail-protocol");
    assert!(
        detail_lines(&summary, 0)
            .iter()
            .map(line_text)
            .any(|line| line.contains(&label) && line.contains("2026-07-28"))
    );
}

#[test]
fn detail_lines_protocol_falls_back_when_unnegotiated() {
    let label = i18n::tr("panel-mcp-detail-protocol");
    let placeholder = i18n::tr("ui-empty");
    assert!(
        detail_lines(&McpServerSummary::default(), 0)
            .iter()
            .map(line_text)
            .any(|line| line.contains(&label) && line.contains(&placeholder))
    );
}

#[test]
fn connected_at_parses_offsets_to_same_instant() {
    // %s（Unix 秒）不受本地时区影响：同一时刻的两种写法必须一致。
    let zulu = format_connected_at("2026-10-11T06:32:05Z", "%s");
    let offset = format_connected_at("2026-10-11T14:32:05+08:00", "%s");
    assert!(zulu.is_some());
    assert_eq!(zulu, offset);
}

#[test]
fn connected_at_rejects_invalid_timestamp() {
    assert_eq!(format_connected_at("not-a-timestamp", "%H:%M"), None);
}

#[test]
fn detail_button_row_shifts_with_error_summary() {
    let with_error = McpServerSummary {
        error_summary: Some("boom".into()),
        ..McpServerSummary::default()
    };
    // 单行错误：错误标题 + 摘要 + 空行 = 3 行（复制按钮与标题同行，不占行）
    assert_eq!(detail_btn_row(&with_error, 0, TEST_WIDTH), 24);
    // 换行行数增长 → 按钮行同步下移（120 字符按 52 宽 = 3 行）
    let long_error = McpServerSummary {
        error_summary: Some("E".repeat(120)),
        ..McpServerSummary::default()
    };
    assert_eq!(detail_error_lines(&long_error), Some(3));
    assert_eq!(detail_btn_row(&long_error, 0, TEST_WIDTH), 26);
}

#[test]
fn cache_status_labels_are_explicit() {
    assert_eq!(
        cache_status_label_key(Some("cache_disabled_by_config")),
        Some("panel-mcp-cache-disabled-config")
    );
    assert_eq!(
        cache_status_label_key(Some("cache_pending")),
        Some("panel-mcp-cache-pending")
    );
    assert_eq!(
        cache_status_label_key(Some("cache_ready")),
        Some("panel-mcp-cache-ready")
    );
    assert_eq!(
        cache_status_label_key(Some("stored_after_fetch")),
        Some("panel-mcp-cache-saved")
    );
    assert_eq!(
        cache_status_label_key(Some("cached")),
        Some("panel-mcp-cache-hit")
    );
    assert_eq!(
        cache_status_label_key(Some("cache_disabled")),
        Some("panel-mcp-cache-disabled")
    );
    assert_eq!(
        cache_status_label_key(Some("live_fetch")),
        Some("panel-mcp-cache-live-fetch")
    );
}

// ── 弹窗前景独占（键盘路由）───────────────────────────────────────────
//
// 事件入口挂载脚手架：`State<T>` 无公开构造 API，句柄只能从真实组件挂载中
// 捕获（与 plugin 面板测试同款 visitor 模式）。

pub(super) struct MountedPanel {
    pub(super) view: State<McpView>,
    pub(super) selected: State<usize>,
    pub(super) detail_key: State<Option<String>>,
    pub(super) detail_btn: State<usize>,
    /// 面板滚动状态（详情鼠标命中的行号偏移来源）。
    pub(super) sv: State<ScrollViewState>,
    pub(super) control: State<super::actions::ControlState>,
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
    static MOUNT_VISITOR: std::cell::RefCell<MountVisitor> =
        const { std::cell::RefCell::new(None) };
}

/// 每帧把当前句柄交给 visitor（不消费）：跨帧用例可在第 N 帧断言前 N-1 帧的效果。
pub(super) fn visit_mounted_panel(panel: MountedPanel) {
    MOUNT_VISITOR.with(|slot| {
        if let Some(visitor) = slot.borrow_mut().as_mut() {
            visitor(panel);
        }
    });
}

/// 测试用面板区域：与 `mount_panel` 的渲染尺寸同量级。
const TEST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 60,
    height: 18,
};

impl MountedPanel {
    /// 走真实事件入口（弹窗让路判定 + 鼠标/键盘分支），视口按测试区域推导。
    fn event(&self, event: Event) -> EventResult {
        let servers = MCP_SERVERS.state().read().clone();
        let viewport = ListViewport::for_content_rows(
            content_rows(Some(TEST_AREA), TEST_AREA.height),
            super::LIST_METRICS,
            servers.len(),
            *self.selected.read(),
        );
        let skills = std::collections::HashMap::new();
        super::handle_mcp_event(
            event,
            &super::McpEventCtx {
                area: Some(TEST_AREA),
                view: self.view,
                selected: self.selected,
                detail_key: self.detail_key,
                detail_btn: self.detail_btn,
                sv: self.sv,
                control: self.control,
                control_scope: super::actions::current_scope(),
                viewport,
                servers: &servers,
                skills_by_server: &skills,
            },
        )
    }
}

/// 挂载面板并渲染 `frames` 帧；visitor 每帧收到 (帧序号, 当前句柄)，返回末帧缓冲。
///
/// effect 在组件体内同步执行、先于 visitor：第 N 帧 visitor 观察到的是本帧
/// 更新阶段收敛后的状态，因此「改状态 → 下一帧断言效果」的用例至少渲染 2 帧。
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
    ratatui_kit::test_util::render_frames(element!(super::McpPanel()), 80, 24, frames)
}

/// 单帧挂载（不需跨帧断言的用例）。
fn mount_panel(mut visitor: impl FnMut(MountedPanel) + 'static) -> Buffer {
    mount_panel_frames(1, move |_, panel| visitor(panel))
}

/// 缓冲全文——断言渲染结果（用户名、状态、提示）用。
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

fn server(name: &str) -> McpServerSummary {
    McpServerSummary {
        name: name.to_string(),
        ..McpServerSummary::default()
    }
}

/// 前景相关 atom 的保存/恢复——断言失败也不污染其它测试。
pub(super) struct ForegroundGuard {
    popup: Option<PopupKind>,
    oauth: Option<OauthNeeded>,
    panel: Option<PanelKind>,
    servers: Vec<McpServerSummary>,
}

impl ForegroundGuard {
    pub(super) fn capture() -> Self {
        Self {
            popup: *POPUP_KIND.state().read(),
            oauth: OAUTH_INFO.state().read().clone(),
            panel: *ACTIVE_PANEL.state().read(),
            servers: MCP_SERVERS.state().read().clone(),
        }
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        *POPUP_KIND.state().write() = self.popup;
        *OAUTH_INFO.state().write() = self.oauth.take();
        *ACTIVE_PANEL.state().write() = self.panel;
        *MCP_SERVERS.state().write() = std::mem::take(&mut self.servers);
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// 回归：OAuth 弹窗（由本面板详情 [ 授权 ] 触发）打开期间，面板 handler 必须
/// 对所有键让路——弹窗与面板同为 High 且同属 root 输入层，面板注册在先，
/// 抢先把键消费掉就是「弹窗渲染出来了，键盘却作用在下层面板」的根因。
#[test]
#[serial]
fn oauth_popup_open_yields_all_keys_to_popup() {
    let _guard = ForegroundGuard::capture();
    // 面板仍激活（写 ACTIVE_PANEL 而非 open_panel：不污染 OPEN_PANELS 面板栈，
    // 否则后续测试关面板时会停在本用例压入的栈顶）
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = Some(PopupKind::OAuth);
    *OAUTH_INFO.state().write() = Some(OauthNeeded {
        server_name: "srv".into(),
        auth_url: "https://example.com/auth".into(),
    });

    mount_panel(|panel| {
        // 详情视图（[ 授权 ] 所在视图）+ 非零选择：面板侧任何抢跑都可见
        *panel.view.write() = McpView::Detail;
        *panel.selected.write() = 1;
        for code in [
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Char('a'),
            KeyCode::Backspace,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
        ] {
            assert_eq!(
                panel.event(key(code)),
                EventResult::Ignored,
                "{code:?} 必须留给弹窗"
            );
        }
        // 面板状态未被抢跑：Esc 没退回列表、←→ 没换按钮、Tab 没翻焦点
        assert_eq!(*panel.view.read(), McpView::Detail);
        assert_eq!(*panel.selected.read(), 1);
        assert_eq!(*panel.detail_btn.read(), 0);
    });
}

/// 守卫不得过度让路：无弹窗时详情 Esc 返回列表、列表 ↑↓ 照常移动选择。
#[test]
#[serial]
fn panel_keeps_keys_when_no_popup() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![
        McpServerSummary {
            name: "a".into(),
            ..McpServerSummary::default()
        },
        McpServerSummary {
            name: "b".into(),
            ..McpServerSummary::default()
        },
    ];

    mount_panel(|panel| {
        *panel.view.write() = McpView::Detail;
        assert_eq!(panel.event(key(KeyCode::Esc)), EventResult::Consumed);
        assert_eq!(*panel.view.read(), McpView::List);

        assert_eq!(panel.event(key(KeyCode::Down)), EventResult::Consumed);
        assert_eq!(*panel.selected.read(), 1);
        assert_eq!(panel.event(key(KeyCode::Up)), EventResult::Consumed);
        assert_eq!(*panel.selected.read(), 0);
    });
}

// ── 动态视口与稳定身份（批次 B）────────────────────────────────────────

/// 视口回归：可见项数由面板内容区行数决定，不再固定为 6。
#[test]
#[serial]
fn visible_items_follow_panel_height() {
    let _guard = ForegroundGuard::capture();
    *MCP_SERVERS.state().write() = (0..12).map(|i| server(&format!("mcpsrv{i:02}"))).collect();

    // 60×10：内容区 8 行，度量 (2,2,2) 放得下 2 项
    let short = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::McpPanel()),
        60,
        10,
        2,
    ));
    assert!(short.contains("mcpsrv00"), "矮面板至少渲染第一项");
    assert!(short.contains("mcpsrv01"), "内容区 8 行放得下 2 项");
    assert!(!short.contains("mcpsrv02"), "第三项必须被视口裁掉");

    // 60×40：内容区 38 行 → 17 项，12 个 server 全部渲染
    let tall = buffer_text(&ratatui_kit::test_util::render_frames(
        element!(super::McpPanel()),
        60,
        40,
        2,
    ));
    assert!(
        tall.contains("mcpsrv11"),
        "面板变高后必须渲染更多项，而不是停在写死的 6 项"
    );
}

/// 列表视图左键点击 = 选中 + 进详情（与 Enter 同语义），命中与绘制同源。
#[test]
#[serial]
fn click_on_row_opens_detail_for_that_server() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![server("alpha"), server("beta")];

    // 面板 60×18：内容行 4..6 = 第二项（header 2 行 + 每项 2 行）
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel(move |panel| {
        let result = panel.event(left_down(6, 5));
        assert_eq!(*panel.selected.read(), 1, "点击必须选中被点的那一项");
        assert_eq!(*panel.view.read(), McpView::Detail);
        assert_eq!(panel.detail_key.read().as_deref(), Some("beta"));
        tx.send(result).expect("测试结果通道仍打开");
    });
    assert_eq!(rx.try_recv().ok(), Some(EventResult::Consumed));
}

/// 最右滚动条列不触发列表动作。
#[test]
#[serial]
fn click_on_scrollbar_column_does_not_open_detail() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![server("alpha"), server("beta")];

    mount_panel(|panel| {
        assert_eq!(panel.event(left_down(6, 59)), EventResult::Consumed);
        assert_eq!(*panel.selected.read(), 0);
        assert_eq!(*panel.view.read(), McpView::List);
        assert!(panel.detail_key.read().is_none());
    });
}

/// 详情滚动后，鼠标命中必须加上 `ScrollViewState` 偏移：滚到可见区的按钮可点，
/// 未滚动时的旧行号不会误触 header 复制按钮。
#[test]
#[serial]
fn detail_click_accounts_for_scroll_offset() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![server("alpha")];

    mount_panel(|panel| {
        *panel.view.write() = McpView::Detail;
        *panel.detail_key.write() = Some("alpha".into());
        let offset = 8u16;
        *panel.sv.write() = ScrollViewState::with_offset(Position::new(0, offset));

        // 默认详情 24 行、按钮在内容行 21（TEST_AREA 60 宽）；滚动 8 行后按钮
        // 落在屏幕内容行 13。先点内容行 1：加偏移后是内容行 9，不得命中任何按钮。
        assert_eq!(
            panel.event(left_down(TEST_AREA.y + 1 + 1, 2)),
            EventResult::Consumed
        );
        assert_eq!(
            *panel.view.read(),
            McpView::Detail,
            "偏移后的旧行号不得命中按钮"
        );

        let btn_row = detail_btn_row(&server("alpha"), 0, TEST_AREA.width);
        let screen_row = TEST_AREA.y + 1 + btn_row - offset;
        assert_eq!(panel.event(left_down(screen_row, 2)), EventResult::Consumed);
        assert_eq!(
            *panel.view.read(),
            McpView::List,
            "滚动后点击 [ 返回 ] 必须生效"
        );
        assert_eq!(panel.sv.read().offset().y, 0, "退出详情必须重置滚动偏移");
    });
}

/// 进入/退出详情都从顶部开始：列表与详情的滚动偏移不得互相污染。
#[test]
#[serial]
fn entering_and_leaving_detail_resets_scroll_offset() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![server("alpha")];

    mount_panel(|panel| {
        *panel.sv.write() = ScrollViewState::with_offset(Position::new(0, 5));
        // 内容行 3 = 第一项的第二行（列表 header 2 行 + 每项 2 行）。
        assert_eq!(panel.event(left_down(4, 5)), EventResult::Consumed);
        assert_eq!(*panel.view.read(), McpView::Detail);
        assert_eq!(panel.sv.read().offset().y, 0, "进入详情必须重置偏移");

        *panel.sv.write() = ScrollViewState::with_offset(Position::new(0, 5));
        assert_eq!(panel.event(key(KeyCode::Esc)), EventResult::Consumed);
        assert_eq!(*panel.view.read(), McpView::List);
        assert_eq!(panel.sv.read().offset().y, 0, "退出详情必须重置偏移");
    });
}

/// 回归：详情内容必须真正撑开 ScrollView 的滚动缓冲区。
///
/// 技能归组只接受 `McpSkill`：其他域（plugin/user 等）即使 fullname 带冒号
/// 也不得混入 server 的技能表。
#[test]
fn skills_grouping_only_accepts_mcp_skill_kind() {
    use crate::kit::slash_completion::SlashActionKind;
    use crate::kit::slash_projection::SlashCommandEntry;

    let entry = |fullname: &str, kind: SlashActionKind| SlashCommandEntry {
        fullname: fullname.into(),
        kind,
        description: format!("{fullname} description"),
        ..SlashCommandEntry::default()
    };
    let entries = vec![
        entry("demo:hello", SlashActionKind::McpSkill),
        entry("demo:other", SlashActionKind::McpSkill),
        // 与 server 域名同名但非 MCP 域：不得混入。
        entry("demo:plugin-command", SlashActionKind::Command),
        entry("demo:user-skill", SlashActionKind::Skill),
        // 无冒号的 MCP skill 不归属任何 server。
        entry("lonely", SlashActionKind::McpSkill),
    ];
    let grouped = super::group_mcp_skills(&entries);
    assert_eq!(grouped.len(), 1);
    let names: Vec<&str> = grouped["demo"]
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(names, vec!["demo:hello", "demo:other"]);
}

/// 稳定身份回归：列表刷新改变目标下标后，详情仍指向进入时的同名 server。
#[test]
#[serial]
fn detail_keeps_same_server_after_list_refresh() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![server("alpha"), server("beta")];

    let buf = mount_panel_frames(3, |frame, panel| match frame {
        0 => {
            // 等价于列表 Enter：进入 beta 详情
            *panel.view.write() = McpView::Detail;
            *panel.detail_key.write() = Some("beta".into());
        }
        1 => {
            // 列表刷新：alpha 下线，beta 从下标 1 变成 0（按下标定位会切到 gamma）
            *MCP_SERVERS.state().write() = vec![server("beta"), server("gamma")];
        }
        _ => {}
    });

    let text = buffer_text(&buf);
    assert!(text.contains("beta"), "详情须显示进入时的 server：{text}");
    assert!(
        !text.contains("gamma"),
        "详情不得被列表下标带到 gamma：{text}"
    );
}

/// 详情目标消失（列表刷新）在 effect 边界收敛回列表，不在渲染里写状态。
#[test]
#[serial]
fn detail_target_disappearing_returns_to_list() {
    let _guard = ForegroundGuard::capture();
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Mcp);
    *POPUP_KIND.state().write() = None;
    *MCP_SERVERS.state().write() = vec![server("alpha"), server("beta")];

    mount_panel_frames(3, |frame, panel| match frame {
        0 => {
            *panel.view.write() = McpView::Detail;
            *panel.detail_key.write() = Some("beta".into());
            *panel.detail_btn.write() = 1;
        }
        1 => {
            assert_eq!(
                *panel.view.read(),
                McpView::Detail,
                "刷新发生在帧间，本帧仍是详情"
            );
            *MCP_SERVERS.state().write() = vec![server("alpha")];
        }
        _ => {
            assert_eq!(*panel.view.read(), McpView::List, "目标消失后必须回到列表");
            assert!(panel.detail_key.read().is_none());
            assert_eq!(*panel.detail_btn.read(), 0);
        }
    });
}
