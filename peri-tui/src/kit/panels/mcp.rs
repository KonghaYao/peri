//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 从 mcp_pool.all_server_infos 派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。只读面板——MCP 配置通过 ~/.claude/settings.json 管理。
//!
//! OAuth 授权入口（详情视图）：列表选中「需要认证」server 按 Enter 进入
//! 详情视图，详情里选 [ 授权 ] 按钮（Enter/鼠标点击）才触发 mcp/oauth_start
//! RPC → host 异步授权 → 弹出 OAuthPopup；[ 返回 ] 或 Esc 回列表。
//!
//! 错误摘要存在时详情 header 提供 [ 复制 ] 按钮：右对齐渲染在标题（server 名/
//! 状态）行右侧，激活后把错误原文写入剪贴板并留在详情视图；共用 ←→ 选择 +
//! Enter 确认 / 鼠标左键点击的按钮机制。窄终端下标题按预算截断，按钮优先保留。

use std::collections::HashMap;
use std::time::Duration;

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, AVAILABLE_SLASH_COMMANDS, LANG_VERSION, MCP_SERVERS, McpServerSummary,
    SERVICE_PROJECTION_ERROR, SERVICE_SNAPSHOT,
};
use crate::kit::focus_router::popup_owns_foreground;
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, left_down};
use crate::kit::text_util::wrap_text;
use crate::truncate::truncate_by_width;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

/// 面板视图：列表 ⇄ OAuth 授权详情。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpView {
    List,
    Detail,
}

#[component]
pub fn McpPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let store = hooks.use_atom(&MCP_SERVERS);
    let projection_error = hooks.use_atom(&SERVICE_PROJECTION_ERROR);
    let servers: Vec<McpServerSummary> = store.read().clone();
    let _ = store;

    let snap_store = hooks.use_atom(&SERVICE_SNAPSHOT);
    let init_phase = snap_store.read().mcp.init_phase;
    let connected_total = snap_store.read().mcp.connected;
    let config_total = snap_store.read().mcp.total;
    let _ = snap_store;
    // 每 server 已注入的 MCP skill 命令数（决策 1：命令面 fullname =
    // `{server}:{skill}`，来自 available_commands_update 投影；discovery
    // 完成 → 注册表 on_change → 广播 → 本 atom 自动刷新，无需轮询）。
    // 命令面首段 = server 名末段小写（mcp_source_key 派生，与 host 侧
    // 同源），查询时按同规则归一。
    let cmd_store = hooks.use_atom(&AVAILABLE_SLASH_COMMANDS);
    let skills_by_server: HashMap<String, usize> = {
        let mut m: HashMap<String, usize> = HashMap::new();
        for entry in cmd_store.read().iter() {
            if let Some((server, _)) = entry.fullname.split_once(':') {
                *m.entry(server.to_string()).or_insert(0) += 1;
            }
        }
        m
    };
    let _ = cmd_store;
    let mcp_domain_key = |name: &str| -> String {
        name.rsplit_once(':')
            .map(|(_, n)| n)
            .unwrap_or(name)
            .to_lowercase()
    };
    let _ = hooks.use_atom(&LANG_VERSION);

    // 视图状态：List ⇄ Detail（OAuth 授权入口）
    let view = hooks.use_state(|| McpView::List);
    // 进入详情时的列表 index（返回列表后保持选中）
    let detail_idx = hooks.use_state(|| 0usize);
    // 详情视图按钮选择：OAuth server 为 0 = 授权、1 = 返回；默认显式落在
    // 返回，避免列表 Enter 与授权 Enter 的含义混杂。
    let detail_btn = hooks.use_state(|| 0usize);
    // 面板绘制区域（上一帧，绝对坐标）——鼠标命中反推行号
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }
    // 面板内容宽度：header 复制按钮右对齐用（渲染与鼠标命中必须同源——两者都取
    // AreaTracker 的同一区域，仅首帧未测量时兜底）。
    let content_width = area.map_or(DETAIL_FALLBACK_WIDTH, |rect| rect.width);

    // 事件闭包另持一份 servers 副本（与渲染端共用同一 atom 副本）
    let servers_for_closure = servers.clone();

    hooks.use_event_handler_with_options(
        EventScope::Current,
        // High：详情视图的 Esc（返回列表）必须先于根层 Normal Esc（关面板）
        // 被消费——同优先级先注册先消费，Normal 会变成死代码（rewind_popup 同款坑）。
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            handle_mcp_event(
                event,
                area,
                view,
                selected,
                detail_idx,
                detail_btn,
                &servers_for_closure,
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        view,
        selected,
        detail_idx,
        detail_btn,
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 2 - footer 2 = 12 行；每项 2 行 → 可见 6 个。
    const VISIBLE_ITEMS: usize = 6;
    let scroll_start = scroll_start_for_selected(sel, servers.len(), VISIBLE_ITEMS);

    if *view.read() == McpView::Detail {
        // ── 详情视图（OAuth 授权入口）──
        if let Some(summary) = servers.get(*detail_idx.read()) {
            lines = build_detail_lines(
                summary,
                *detail_btn.read(),
                &theme_def.read(),
                content_width,
            );
        } else {
            // detail_idx 失效（列表被刷新缩短）：回到列表
            *view.write() = McpView::List;
        }
    } else {
        // ── 列表视图 ──
        // 摘要头：init phase / connected / total
        let phase_label = match init_phase {
            crate::kit::atoms::McpInitPhase::Pending => i18n::tr("panel-mcp-phase-pending"),
            crate::kit::atoms::McpInitPhase::Initializing => {
                i18n::tr("panel-mcp-phase-initializing")
            }
            crate::kit::atoms::McpInitPhase::Ready => i18n::tr("panel-mcp-phase-ready"),
            crate::kit::atoms::McpInitPhase::Failed => i18n::tr("panel-mcp-phase-failed"),
        };
        lines.push(Line::from(vec![
            Span::styled(
                i18n::tr("panel-mcp-pool-label"),
                Style::new().fg(theme_def.read().semantic.text.muted),
            ),
            Span::styled(
                phase_label.clone(),
                Style::new()
                    .fg(theme_def.read().semantic.border.active)
                    .bold(),
            ),
            Span::styled(
                i18n::tr_args(
                    "panel-mcp-connected",
                    &[
                        (
                            "connected".to_string(),
                            FluentValue::from(connected_total as i64),
                        ),
                        ("total".to_string(), FluentValue::from(config_total as i64)),
                    ],
                ),
                Style::new().fg(theme_def.read().semantic.text.primary),
            ),
        ]));
        lines.push(Line::from(""));

        if servers.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-mcp-empty"),
                Style::new().fg(theme_def.read().semantic.text.muted),
            )]));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-mcp-empty-hint"),
                Style::new().fg(theme_def.read().semantic.text.muted),
            )]));
        } else {
            for (i, s) in servers
                .iter()
                .enumerate()
                .skip(scroll_start)
                .take(VISIBLE_ITEMS)
            {
                let is_selected = i == sel;
                let cursor = if is_selected { ">" } else { " " };
                let name_style = if is_selected {
                    Style::new()
                        .fg(theme_def.read().component.panel.title)
                        .bold()
                } else {
                    Style::new().fg(theme_def.read().semantic.text.primary)
                };
                let (status_icon, status_color) = derive_status_style(&s.status);
                // OAuth 待授权：状态图标用 warning 色强调 + name 后加徽标
                let auth_badge = if s.needs_auth {
                    format!(" {}", i18n::tr("panel-mcp-needs-auth"))
                } else {
                    String::new()
                };
                let badge_style = Style::new().fg(theme_def.read().semantic.status.warning);

                let status_spans = vec![
                    Span::styled(
                        format!(" {} ", cursor),
                        Style::new().fg(theme_def.read().component.panel.title),
                    ),
                    Span::styled(s.name.clone(), name_style),
                    Span::styled(auth_badge, badge_style),
                    Span::styled(format!("  {}", status_icon), Style::new().fg(status_color)),
                    Span::styled(format!(" {}", s.status), Style::new().fg(status_color)),
                ];
                lines.push(Line::from(status_spans));

                let mut detail_spans = vec![Span::styled(
                    i18n::tr_args(
                        "panel-mcp-server-detail",
                        &[
                            (
                                "transport".to_string(),
                                FluentValue::from(s.transport.as_str()),
                            ),
                            ("count".to_string(), FluentValue::from(s.tools_count as i64)),
                            (
                                "skills".to_string(),
                                FluentValue::from(
                                    skills_by_server
                                        .get(&mcp_domain_key(&s.name))
                                        .copied()
                                        .unwrap_or(0) as i64,
                                ),
                            ),
                        ],
                    ),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                )];
                if let Some(version) = &s.version {
                    detail_spans.push(Span::styled(
                        format!("  ·  {version}"),
                        Style::new().fg(theme_def.read().semantic.text.dim),
                    ));
                }
                lines.push(Line::from(detail_spans));
            }
        }

        lines.push(Line::from(""));
        // 底部提示统一为列表进入详情；授权只能在详情页显式选择按钮后触发。
        lines.push(
            Line::from(i18n::tr("panel-mcp-list-hint")).fg(theme_def.read().semantic.text.dim),
        );
    }

    if let Some(error) = projection_error.read().as_ref() {
        lines.push(Line::styled(
            error.clone(),
            Style::new().fg(theme_def.read().semantic.status.warning),
        ));
    }
    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Mcp, hooks.use_previous_size(), sv);

    panel_shell!(PanelKind::Mcp, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                state: Some(sv),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

/// 面板事件处理（详情视图鼠标命中 + 键盘导航）。
///
/// 判定集中在这里、组件只做注册，测试可经 `tests::mount_panel` 捕获真实句柄后
/// 直接投喂事件（`State<T>` 无公开构造 API）。
///
/// **弹窗让路**：弹窗 handler 与面板 handler 同为 `EventPriority::High` 且同属
/// root 输入层，层内投递序只按注册序——本面板注册在 `PopupOverlay` 之前，若继续
/// 消费按键，OAuth/HITL 等弹窗就收不到任何键（focus_router 目标优先级
/// Popup > Panel）。故弹窗占用前景时整体 Ignored。
fn handle_mcp_event(
    event: Event,
    area: Option<Rect>,
    view: State<McpView>,
    selected: State<usize>,
    detail_idx: State<usize>,
    detail_btn: State<usize>,
    servers: &[McpServerSummary],
) -> EventResult {
    if popup_owns_foreground() {
        return EventResult::Ignored;
    }

    // ── 鼠标：详情视图按钮行左键点击 = 激活按钮 ──
    if let Event::Mouse(mouse) = &event {
        if *view.read() != McpView::Detail {
            return EventResult::Ignored;
        }
        let Some((row, col)) = left_down(mouse) else {
            return EventResult::Ignored;
        };
        let Some(area) = area else {
            return EventResult::Consumed;
        };
        // 顶部边框行不可点
        if row < area.y.saturating_add(1) {
            return EventResult::Consumed;
        }
        let content_row = row.saturating_sub(area.y).saturating_sub(1);
        let Some(summary) = servers.get(*detail_idx.read()) else {
            return EventResult::Consumed;
        };
        // 按钮命中：行号与列偏移取自渲染同源的 detail_buttons（header 右对齐
        // 复制按钮与底行按钮），列按显示宽度计算（CJK label 双宽）
        for button in detail_buttons(summary, area.width) {
            if content_row != button.row {
                continue;
            }
            let x = area.x.saturating_add(button.col);
            if col >= x && col < x.saturating_add(button.hit_width) {
                activate_detail_btn(button.sel, summary, &view, &detail_btn);
                return EventResult::Consumed;
            }
        }
        return EventResult::Consumed;
    }

    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }

    if *view.read() == McpView::Detail {
        // ── 详情视图：←→ 选按钮，Enter 激活，Esc 返回列表 ──
        match key.code {
            KeyCode::Esc => {
                *view.write() = McpView::List;
            }
            KeyCode::Left | KeyCode::Right => {
                let Some(summary) = servers.get(*detail_idx.read()) else {
                    return EventResult::Consumed;
                };
                // 选择索引空间 = detail_actions（header 复制 → 底行 授权/返回）；
                // 只有一个按钮（[ 返回 ]）时无选择余地。
                let count = detail_actions(summary).len();
                if count > 1 {
                    let mut b = detail_btn.write();
                    *b = if key.code == KeyCode::Right {
                        (*b + 1) % count
                    } else {
                        (*b + count - 1) % count
                    };
                }
            }
            KeyCode::Enter => {
                let b = *detail_btn.read();
                if let Some(summary) = servers.get(*detail_idx.read()) {
                    activate_detail_btn(b, summary, &view, &detail_btn);
                }
            }
            _ => {}
        }
        return EventResult::Consumed;
    }

    // ── 列表视图 ──
    match key.code {
        KeyCode::Esc => close_panel(),
        KeyCode::Enter => {
            let servers = MCP_SERVERS.state().read().clone();
            let sel = *selected.read();
            if servers.get(sel).is_some() {
                *detail_idx.write() = sel;
                *detail_btn.write() = default_detail_btn(&servers[sel]);
                *view.write() = McpView::Detail;
            }
        }
        KeyCode::Up => {
            let mut s = selected.write();
            *s = previous_selection(*s);
        }
        KeyCode::Down => {
            let mut s = selected.write();
            let count = MCP_SERVERS.state().read().len();
            if count > 0 {
                *s = next_selection(*s, count);
            }
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 详情视图正文换行宽度（错误摘要与 URL 共用）。
///
/// 面板实际宽度未知且行号契约（鼠标命中反推行号）必须与终端宽度无关，故取固定
/// 值：正文缩进 4 列 + 52 列 ≈ 56 列，覆盖默认面板宽度。与 URL 同宽，使两块
/// 长文本的换行行为一致。
const DETAIL_WRAP_WIDTH: usize = 52;

/// 详情视图 header（标题）行号——server 名/状态/版本/授权徽标与右对齐复制按钮
/// 同行。鼠标命中反推用；列偏移由 [`detail_buttons`] 按面板宽度算出。
const DETAIL_HEADER_ROW: u16 = 1;

/// header 右对齐元素预留的右边缘列数：ScrollView 的垂直滚动条画在内容最右列
/// （内容溢出时它占该列），右对齐的复制按钮必须避开，否则会被滚动条覆盖。
const DETAIL_RIGHT_RESERVE: u16 = 1;

/// 标题与右对齐复制按钮之间的最小间距（列）：两者都可见的前提。
const DETAIL_HEADER_MIN_GAP: u16 = 1;

/// 组件区域尚未测量（首帧）时的内容宽度兜底——仅影响该帧的右对齐位置。
const DETAIL_FALLBACK_WIDTH: u16 = 80;

/// 详情视图可激活操作，视觉顺序：header 复制 → 底行 授权 → 底行 返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAction {
    CopyError,
    Auth,
    Back,
}

impl DetailAction {
    fn label(self) -> String {
        match self {
            Self::CopyError => i18n::tr("panel-mcp-detail-btn-copy"),
            Self::Auth => i18n::tr("panel-mcp-detail-btn-auth"),
            Self::Back => i18n::tr("panel-mcp-detail-btn-back"),
        }
    }

    /// 按钮渲染文本：复制按钮沿用 chat md 复制按钮的「左右各 1 空格」块形式
    /// （`message_area/render.rs::copy_button_line`）；其余沿用 chat 选项按钮的
    /// `[ label ]` 形式（`message_area/render/interaction.rs`）。
    fn text(self, label: &str) -> String {
        match self {
            Self::CopyError => format!(" {label} "),
            Self::Auth | Self::Back => format!("[ {label} ]"),
        }
    }
}

/// 详情视图按钮：操作 + 渲染文本 + 选择索引（`detail_actions` 下标）+ 所在
/// 内容区行号/列偏移 + 命中宽度。渲染、鼠标命中与键盘选择共用这一份布局，
/// 避免三处各自维护偏移而漂移。
#[derive(Debug, Clone)]
struct DetailButton {
    action: DetailAction,
    /// 渲染文本（随操作形态不同：` 复制 ` / `[ 授权 ]`）
    text: String,
    sel: usize,
    row: u16,
    col: u16,
    /// 鼠标命中宽度（列）。右对齐按钮在极窄终端被面板裁切时收窄到可见范围，
    /// 保证命中区间落在面板内。
    hit_width: u16,
}

impl DetailButton {
    /// 渲染宽度（显示宽度，CJK label 为双宽字符）。仅布局测试断言使用（生产路径
    /// 在 `detail_buttons` 内按 text 直接计宽），故 cfg(test)。
    #[cfg(test)]
    fn width(&self) -> u16 {
        unicode_width::UnicodeWidthStr::width(self.text.as_str()) as u16
    }
}

/// 按钮样式——复用 chat 消息区按钮的视觉（样式为组件内私有内联实现，故在此
/// 精确复刻构造并注明出处；未改动 chat 组件）：
///
/// - 复制（header）：`message_area/render.rs:91` `copy_button_line` 的 md 复制
///   按钮——`fg(accent) + REVERSED` 反色块。选中态叠加 `BOLD`：chat「当前项」
///   样式（`message_area/mod.rs:719`）是 `bg(surface.selection) + BOLD`，反色块下
///   bg 不可见，故取其中的 BOLD 增量。
/// - 授权/返回（底行）：`message_area/render/interaction.rs` 选项（权限）按钮——
///   默认 `fg(text.primary)`，选中态同 `message_area/mod.rs:719` 的
///   `bg(surface.selection) + BOLD`（与改动前的 MCP 底行按钮一致）。
fn button_style(
    action: DetailAction,
    selected: bool,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Style {
    let semantic = &theme.semantic;
    match action {
        DetailAction::CopyError => {
            let style = Style::default()
                .fg(semantic.accent)
                .add_modifier(Modifier::REVERSED);
            if selected {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }
        DetailAction::Auth | DetailAction::Back => {
            let style = Style::default().fg(semantic.text.primary);
            if selected {
                style
                    .bg(semantic.surface.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }
    }
}

/// 详情视图按钮的选择索引空间（视觉顺序）：[复制?] → [授权?] → [返回]。
///
/// 无错误摘要时不显示复制按钮——没有可复制的内容，且保持常见（连接失败以外的）
/// 详情布局与行号契约不变。
fn detail_actions(s: &McpServerSummary) -> Vec<DetailAction> {
    let mut actions = Vec::with_capacity(3);
    if s.error_summary.is_some() {
        actions.push(DetailAction::CopyError);
    }
    if s.needs_auth {
        actions.push(DetailAction::Auth);
    }
    actions.push(DetailAction::Back);
    actions
}

/// 选择索引 → 操作；索引越界（详情打开期间列表被刷新）返回 None。
fn detail_action_at(s: &McpServerSummary, idx: usize) -> Option<DetailAction> {
    detail_actions(s).into_iter().nth(idx)
}

/// 进入详情时的默认选中：显式落在 [ 返回 ]，避免 Enter 误触授权/复制。
fn default_detail_btn(s: &McpServerSummary) -> usize {
    detail_actions(s).len().saturating_sub(1)
}

/// 详情视图按钮布局（行号 + 列偏移，列从内容区左边缘计）。
///
/// `width` 是面板内容宽度：复制按钮右对齐（`width − 滚动条预留 − 按钮宽`），
/// 底行按钮左起排布。渲染与鼠标命中必须传同一个宽度。
fn detail_buttons(s: &McpServerSummary, width: u16) -> Vec<DetailButton> {
    let bottom_row = detail_btn_row(s);
    let mut buttons: Vec<DetailButton> = Vec::with_capacity(3);
    // 底行按钮从左到右排列：`[ label ]` 之间 2 空格
    let mut bottom_col = 0u16;
    for (sel, action) in detail_actions(s).into_iter().enumerate() {
        let text = action.text(&action.label());
        let text_width = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
        let header = action == DetailAction::CopyError;
        // 右对齐：按钮右缘落在滚动条预留列之前；极窄终端下贴左列渲染（被面板
        // 裁切也要保持可见可点击——按钮优先于标题）
        let (row, col) = if header {
            (
                DETAIL_HEADER_ROW,
                width.saturating_sub(DETAIL_RIGHT_RESERVE + text_width),
            )
        } else {
            (bottom_row, bottom_col)
        };
        let hit_width = if header {
            text_width.min(width.saturating_sub(col))
        } else {
            text_width
        };
        if !header {
            bottom_col = col + text_width + 2;
        }
        buttons.push(DetailButton {
            action,
            text,
            sel,
            row,
            col,
            hit_width,
        });
    }
    buttons
}

/// 按显示宽度截断样式化片段（保留逐段样式；超长补省略号）。
///
/// 省略号可能多占 1 列——调用方按「最小间距」吸收该溢出。
fn truncate_segments(segments: &[(String, Style)], max_width: u16) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(segments.len());
    let mut remaining = max_width as usize;
    for (text, style) in segments {
        if remaining == 0 {
            break;
        }
        let width = unicode_width::UnicodeWidthStr::width(text.as_str());
        if width <= remaining {
            spans.push(Span::styled(text.clone(), *style));
            remaining -= width;
        } else {
            let cut = truncate_by_width(text, remaining);
            remaining =
                remaining.saturating_sub(unicode_width::UnicodeWidthStr::width(cut.as_str()));
            spans.push(Span::styled(cut, *style));
        }
    }
    spans
}

/// 渲染 header（标题）行：标题片段 +（有错误摘要时）右对齐的复制按钮。
///
/// 窄终端下按键优先：标题截断到「按钮起始列 − 最小间距」，按钮始终渲染并保持
/// 右对齐（`DetailButton::col` 与鼠标命中同源）；没有按钮时原样渲染标题。
fn render_header_row(
    title: &[(String, Style)],
    copy: Option<&DetailButton>,
    sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Line<'static> {
    let Some(button) = copy else {
        return Line::from(
            title
                .iter()
                .map(|(text, style)| Span::styled(text.clone(), *style))
                .collect::<Vec<_>>(),
        );
    };
    let mut spans = truncate_segments(title, button.col.saturating_sub(DETAIL_HEADER_MIN_GAP));
    let used: u16 = spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()) as u16)
        .sum();
    spans.push(Span::raw(
        " ".repeat(button.col.saturating_sub(used) as usize),
    ));
    spans.push(Span::styled(
        button.text.clone(),
        button_style(button.action, button.sel == sel, theme),
    ));
    Line::from(spans)
}

/// 详情视图底行按钮行号（渲染 / 鼠标命中反推共用契约）。
///
/// 内容区行号 0 起：空行、标题（+右对齐复制按钮）、cache、protocol、connected、
/// 空行、[错误标题 + 错误×n + 空行]、URL 标签、URL×n、空行 → 按钮行
/// = 8 + url_lines + (错误行数 + 2)。复制按钮与标题同行，不增加行数。
fn detail_btn_row(s: &McpServerSummary) -> u16 {
    let url_lines = s
        .url
        .as_ref()
        .map(|url| wrap_text(url, DETAIL_WRAP_WIDTH).len() as u16)
        .unwrap_or(1);
    let error_block = detail_error_lines(s)
        .map(|lines| lines as u16 + 2)
        .unwrap_or(0);
    8 + url_lines + error_block
}

/// 错误摘要换行后的行数；None = 无错误摘要。
fn detail_error_lines(s: &McpServerSummary) -> Option<usize> {
    s.error_summary
        .as_deref()
        .map(|error| wrap_text(error, DETAIL_WRAP_WIDTH).len())
}

/// 渲染底行按钮行：`DetailButton::col` 与 `render_button_row` 的输出列一一对应
/// （左起排布、按钮间 2 空格），样式见 [`button_style`]。
fn render_button_row(
    buttons: &[DetailButton],
    sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(buttons.len() * 2);
    for (i, button) in buttons.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            button.text.clone(),
            button_style(button.action, button.sel == sel, theme),
        ));
    }
    Line::from(spans)
}

/// RFC3339（UTC）时间戳 → 本地时间指定格式；解析失败返回 None（界面不显示）。
fn format_connected_at(value: &str, fmt: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|utc| utc.with_timezone(&chrono::Local).format(fmt).to_string())
}

/// 激活详情视图按钮：索引对应 `detail_actions`（header 复制 → 底行 授权/返回）。
/// 键盘 Enter / 鼠标左键点击共用。复制留在详情视图（错误原文仍需对照阅读）；
/// 授权/返回回到列表。
fn activate_detail_btn(
    idx: usize,
    summary: &McpServerSummary,
    view: &ReactiveHandle<McpView, SingleWaker>,
    detail_btn: &ReactiveHandle<usize, SingleWaker>,
) {
    match detail_action_at(summary, idx) {
        Some(DetailAction::CopyError) => copy_error_summary(summary),
        Some(DetailAction::Auth) => {
            start_oauth(summary.name.clone());
            *detail_btn.write() = 0;
            *view.write() = McpView::List;
        }
        // 索引失效（详情打开期间列表被刷新）等同 [ 返回 ]
        Some(DetailAction::Back) | None => {
            *detail_btn.write() = 0;
            *view.write() = McpView::List;
        }
    }
}

/// [ 复制 ] 按钮的载荷：错误原文（不截断、不重排——换行只影响渲染）。
fn detail_copy_text(s: &McpServerSummary) -> Option<&str> {
    s.error_summary.as_deref()
}

/// 复制错误原文到剪贴板：本地写系统剪贴板，SSH/tmux 走 OSC 52
/// （`kit::clipboard` 统一选择后端）。成功/失败都发状态栏通知——静默失败会让
/// 按钮表现为「点了没反应」。
fn copy_error_summary(s: &McpServerSummary) {
    let Some(text) = detail_copy_text(s) else {
        return;
    };
    match crate::kit::clipboard::copy_text(text) {
        Ok(()) => notify(i18n::tr("panel-mcp-detail-copied")),
        Err(error) => {
            tracing::warn!(error = ?error, "mcp panel: copy error summary failed");
            notify(i18n::tr_args(
                "panel-mcp-detail-copy-failed",
                &[("error".into(), error.to_string().into())],
            ));
        }
    }
}

fn cache_status_label(status: Option<&str>) -> Option<String> {
    cache_status_label_key(status).map(i18n::tr)
}

fn cache_status_label_key(status: Option<&str>) -> Option<&'static str> {
    match status {
        Some("version_cached") => Some("panel-mcp-cache-version-hit"),
        Some("mcpp_cached") => Some("panel-mcp-cache-protocol-hit"),
        Some("cached") => Some("panel-mcp-cache-hit"),
        Some("stored_after_fetch") => Some("panel-mcp-cache-saved"),
        Some("cache_ready") => Some("panel-mcp-cache-ready"),
        Some("cache_disabled") => Some("panel-mcp-cache-disabled"),
        Some("cache_disabled_by_config") => Some("panel-mcp-cache-disabled-config"),
        Some("cache_disabled_dynamic") => Some("panel-mcp-cache-disabled-dynamic"),
        Some("cache_pending") => Some("panel-mcp-cache-pending"),
        Some("live_fetch") => Some("panel-mcp-cache-live-fetch"),
        _ => None,
    }
}

/// 构造详情视图内容行（纯函数，测试友好）。
fn build_detail_lines(
    s: &McpServerSummary,
    btn_sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
    width: u16,
) -> Vec<Line<'static>> {
    let semantic = &theme.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(""));
    // 标题（header）：server 名 + 状态 + 版本 + 授权徽标 [+ 右对齐复制按钮]。
    // 片段保留逐段样式，供窄终端下按预算截断（见 render_header_row）。
    let (status_icon, status_color) = derive_status_style(&s.status);
    let mut title: Vec<(String, Style)> = vec![
        (
            format!("  {}", s.name),
            Style::new().fg(theme.component.panel.title).bold(),
        ),
        (format!("  {}", status_icon), Style::new().fg(status_color)),
        (format!(" {}", s.status), Style::new().fg(status_color)),
    ];
    if let Some(version) = &s.version {
        title.push((format!(" · {version}"), Style::new().fg(semantic.text.dim)));
    }
    if s.needs_auth {
        title.push((
            format!("  {}", i18n::tr("panel-mcp-needs-auth")),
            Style::new().fg(semantic.status.warning),
        ));
    }
    // 复制按钮（仅错误摘要存在时；无错误无可复制内容）与底行按钮共用选择索引，
    // 右对齐渲染在 header 行（行号/列偏移由 detail_buttons 单一权威给出）。
    let buttons = detail_buttons(s, width);
    let copy_button = buttons
        .first()
        .filter(|button| button.action == DetailAction::CopyError);
    lines.push(render_header_row(&title, copy_button, btn_sel, theme));
    let cache_label = cache_status_label(s.cache_status.as_deref())
        .unwrap_or_else(|| i18n::tr("panel-mcp-detail-cache-none"));
    lines.push(
        Line::from(vec![
            Span::styled(
                format!("  {} ", i18n::tr("panel-mcp-detail-cache")),
                Style::new().fg(semantic.text.dim),
            ),
            Span::styled(cache_label, Style::new().fg(semantic.text.primary)),
        ])
        .fg(semantic.text.dim),
    );
    // MCP 协议版本（rmcp 协商结果，如 2026-07-28）；未协商出显示占位。
    let protocol_label = s
        .protocol_version
        .clone()
        .unwrap_or_else(|| i18n::tr("ui-empty"));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} ", i18n::tr("panel-mcp-detail-protocol")),
            Style::new().fg(semantic.text.dim),
        ),
        Span::styled(protocol_label, Style::new().fg(semantic.text.primary)),
    ]));
    // 连接时间（RFC3339 → 本地完整时间）；未成功连接显示占位。
    let connected_label = s
        .connected_at
        .as_deref()
        .and_then(|value| format_connected_at(value, "%Y-%m-%d %H:%M:%S"))
        .unwrap_or_else(|| i18n::tr("ui-empty"));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {} ", i18n::tr("panel-mcp-detail-connected")),
            Style::new().fg(semantic.text.dim),
        ),
        Span::styled(connected_label, Style::new().fg(semantic.text.primary)),
    ]));
    lines.push(Line::from(""));
    if let Some(error) = &s.error_summary {
        lines.push(
            Line::from(format!("  {}", i18n::tr("panel-mcp-detail-error")))
                .fg(semantic.status.error),
        );
        // 长错误按面板可用宽度换行完整展示（行数与 detail_btn_row 同源）
        for error_line in wrap_text(error, DETAIL_WRAP_WIDTH) {
            lines.push(Line::from(format!("    {error_line}")).fg(semantic.text.muted));
        }
        lines.push(Line::from(""));
    }
    // URL（HTTP 传输）完整换行展示
    lines.push(Line::from(format!("  {}", i18n::tr("panel-mcp-detail-url"))).fg(semantic.text.dim));
    match &s.url {
        Some(url) => {
            for url_line in wrap_text(url, DETAIL_WRAP_WIDTH) {
                lines.push(Line::from(format!("    {url_line}")).fg(semantic.text.primary));
            }
        }
        None => {
            lines.push(Line::from(format!("    ({})", i18n::tr("ui-empty"))).fg(semantic.text.dim));
        }
    }
    lines.push(Line::from(""));
    // 按钮行：header 复制按钮之外的操作（[授权?] [返回]），列偏移与鼠标命中同源
    let bottom_buttons: Vec<DetailButton> = buttons
        .into_iter()
        .filter(|button| button.action != DetailAction::CopyError)
        .collect();
    lines.push(render_button_row(&bottom_buttons, btn_sel, theme));
    lines.push(Line::from(""));
    lines.push(Line::from(i18n::tr("panel-mcp-detail-hint")).fg(semantic.text.dim));
    lines
}

fn derive_status_style(status: &str) -> (String, ratatui::style::Color) {
    if status.contains("connected") {
        (
            i18n::tr("panel-mcp-icon-connected"),
            THEME_ATOM.state().read().semantic.status.success,
        )
    } else if status.contains("error") || status.contains("failed") {
        (
            i18n::tr("panel-mcp-icon-error"),
            THEME_ATOM.state().read().semantic.status.error,
        )
    } else {
        (
            i18n::tr("panel-mcp-icon-unknown"),
            THEME_ATOM.state().read().semantic.text.muted,
        )
    }
}
fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

/// 状态栏通知（6s 过期）——复制结果与 OAuth 启动失败必须对用户可见，不能只留日志。
fn notify(message: String) {
    crate::kit::atoms::NOTIFICATION.set(Some(crate::kit::atoms::Notification {
        message,
        until: peri_time::monotonic_now() + Duration::from_secs(6),
    }));
}

/// 发起 OAuth 授权（详情视图 [ 授权 ] 按钮）：`mcp/oauth_start` RPC → host pool
/// spawn_oauth_flow → OauthNeeded 事件 → TUI 弹出授权 popup。
///
/// 会话未就绪或 RPC 被拒（capability 协商不符、缺 flow_id 等）时给出状态栏
/// 通知：静默失败会让 [ 授权 ] 按钮表现为「点击没反应」。
fn start_oauth(server_name: String) {
    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
        let client = client_handle.clone();
        let Some(session_id) = client.current_session_id() else {
            tracing::warn!(target: "mcp-panel", "no active session, oauth_start skipped");
            notify(i18n::tr("panel-mcp-oauth-start-no-session"));
            return;
        };
        tokio::spawn(async move {
            let params = serde_json::json!({ "server_name": server_name, "sessionId":session_id });
            match client.send_raw_request("mcp/oauth_start", params).await {
                Err(e) => {
                    tracing::warn!(error = %e, "mcp/oauth_start RPC failed");
                    notify(i18n::tr_args(
                        "panel-mcp-oauth-start-failed",
                        &[("error".into(), e.message.into())],
                    ));
                }
                Ok(response) => {
                    if let Some(status) = oauth_start_failure_message(&response) {
                        tracing::warn!(status = %status, "mcp/oauth_start not started");
                        notify(i18n::tr_args(
                            "panel-mcp-oauth-start-failed",
                            &[("error".into(), status.into())],
                        ));
                    }
                }
            }
        });
    } else {
        tracing::warn!(target: "mcp-panel", "ACP_CLIENT_HANDLE not set, oauth_start skipped");
    }
}

/// `Ok` 响应的失败判定：host 在已有活跃授权流程时返回 `success=false`
/// （status=conflict）而非 RPC 错误——只检查 `Err` 分支会让 [ 授权 ] 按钮
/// 再次静默无反馈。返回需展示的 status（缺失时保底 `conflict`）。
fn oauth_start_failure_message(response: &serde_json::Value) -> Option<String> {
    if response.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
        Some(
            response
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("conflict")
                .to_string(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DETAIL_HEADER_ROW, DETAIL_RIGHT_RESERVE, DETAIL_WRAP_WIDTH, DetailAction, McpView,
        build_detail_lines, button_style, cache_status_label_key, default_detail_btn,
        detail_action_at, detail_actions, detail_btn_row, detail_buttons, detail_copy_text,
        detail_error_lines, format_connected_at,
    };
    use crate::i18n;
    use crate::kit::atoms::McpServerSummary;
    use crate::kit::text_util::wrap_text;
    use ratatui::style::{Modifier, Style};
    use unicode_width::UnicodeWidthStr;

    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms::{ACTIVE_PANEL, MCP_SERVERS, OAUTH_INFO, POPUP_KIND, PopupKind};
    use peri_acp_types::event_data::OauthNeeded;
    use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui_kit::prelude::{EventResult, State, element};
    use ratatui_kit::ratatui::layout::Rect;
    use serial_test::serial;

    /// 测试用面板内容宽度（82 列 + 滚动条预留 → 默认面板宽度量级）。
    const TEST_WIDTH: u16 = 60;

    fn detail_lines(
        summary: &McpServerSummary,
        btn_sel: usize,
    ) -> Vec<ratatui::text::Line<'static>> {
        build_detail_lines(
            summary,
            btn_sel,
            &peri_theme::builtin::dark_theme(),
            TEST_WIDTH,
        )
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// 渲染出的行下标（needle 为按钮渲染文本——复制按钮无方括号、底行按钮带方括号）。
    fn rendered_row(lines: &[ratatui::text::Line<'_>], needle: &str) -> usize {
        lines
            .iter()
            .position(|line| line_text(line).contains(needle))
            .unwrap_or_else(|| panic!("[{needle}] 必须渲染"))
    }

    fn copy_button(summary: &McpServerSummary, width: u16) -> super::DetailButton {
        detail_buttons(summary, width)
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
        assert_eq!(detail_btn_row(&McpServerSummary::default()), 9);
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
            assert_eq!(rendered, detail_btn_row(&summary) as usize);
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
        for button in detail_buttons(&summary, TEST_WIDTH) {
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
            detail_buttons(&summary, TEST_WIDTH).len(),
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
        assert_eq!(detail_btn_row(&with_error), 12);
        // 换行行数增长 → 按钮行同步下移（120 字符按 52 宽 = 3 行）
        let long_error = McpServerSummary {
            error_summary: Some("E".repeat(120)),
            ..McpServerSummary::default()
        };
        assert_eq!(detail_error_lines(&long_error), Some(3));
        assert_eq!(detail_btn_row(&long_error), 14);
    }

    #[test]
    fn cache_status_labels_are_explicit() {
        assert_eq!(
            cache_status_label_key(Some("cache_disabled_by_config")),
            Some("panel-mcp-cache-disabled-config")
        );
        assert_eq!(
            cache_status_label_key(Some("cache_disabled_dynamic")),
            Some("panel-mcp-cache-disabled-dynamic")
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
        pub(super) detail_idx: State<usize>,
        pub(super) detail_btn: State<usize>,
    }

    type MountVisitor = Option<Box<dyn FnOnce(MountedPanel)>>;

    thread_local! {
        static MOUNT_VISITOR: std::cell::RefCell<MountVisitor> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn visit_mounted_panel(panel: MountedPanel) {
        let visitor = MOUNT_VISITOR.with(|slot| slot.borrow_mut().take());
        if let Some(visitor) = visitor {
            visitor(panel);
        }
    }

    impl MountedPanel {
        /// 走真实事件入口（弹窗让路判定 + 鼠标/键盘分支），区域固定为面板量级。
        fn event(&self, event: Event) -> EventResult {
            let servers = MCP_SERVERS.state().read().clone();
            super::handle_mcp_event(
                event,
                Some(Rect::new(0, 0, 60, 18)),
                self.view,
                self.selected,
                self.detail_idx,
                self.detail_btn,
                &servers,
            )
        }
    }

    fn mount_panel(visitor: impl FnOnce(MountedPanel) + 'static) {
        MOUNT_VISITOR.with(|slot| {
            assert!(slot.borrow().is_none(), "visitor 必须在挂载中被消费");
            *slot.borrow_mut() = Some(Box::new(visitor));
        });
        let _ = ratatui_kit::test_util::render_frame(element!(super::McpPanel()), 80, 24);
    }

    /// 前景相关 atom 的保存/恢复——断言失败也不污染其它测试。
    struct ForegroundGuard {
        popup: Option<PopupKind>,
        oauth: Option<OauthNeeded>,
        panel: Option<PanelKind>,
        servers: Vec<McpServerSummary>,
    }

    impl ForegroundGuard {
        fn capture() -> Self {
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
}
