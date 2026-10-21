//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 经 ACP `mcp/list` 查询派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。普通 MCP 的重启/关闭经 ACP 会话控制面。
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
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, hit_item, is_scrollbar_column, left_down};
use crate::kit::panels::shared::list::{ListMetrics, ListViewport, content_rows};
use crate::kit::slash_completion::SlashActionKind;
use crate::kit::slash_projection::SlashCommandEntry;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

mod actions;
mod detail;
use self::detail::{
    DETAIL_FALLBACK_WIDTH, DetailAction, SkillEntry, build_detail_lines, default_detail_btn,
    derive_status_style, detail_action_at, detail_actions, detail_buttons,
};

/// 面板视图：列表 ⇄ OAuth 授权详情。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpView {
    List,
    Detail,
}

/// 列表视图行度量：header 2 行（池摘要、空行）+ 每项 2 行（名称行、详情行）
/// + footer 2 行（空行、提示）。
const LIST_METRICS: ListMetrics = ListMetrics::new(2, 2, 2);

/// 详情目标解析——按 server 名（稳定身份）在最新列表中定位。
///
/// 列表刷新会改变下标，详情不能靠进入时的 index 再取对象；资源消失时返回 `None`，
/// 由更新阶段的 effect 收敛视图，不在渲染里写状态。
fn detail_server<'a>(
    servers: &'a [McpServerSummary],
    key: &Option<String>,
) -> Option<&'a McpServerSummary> {
    let key = key.as_deref()?;
    servers.iter().find(|s| s.name == key)
}

/// server 名 → 命令面域名（末段小写；与 host 侧 `mcp_source_key` 同源）。
fn server_domain_key(name: &str) -> String {
    name.rsplit_once(':')
        .map(|(_, domain)| domain)
        .unwrap_or(name)
        .to_lowercase()
}

/// 命令面投影 → 每 server 的 MCP skill 表。
///
/// 只接受 [`SlashActionKind::McpSkill`]：其他域（如 `plugin:*`）的 fullname 同样
/// 可能含冒号，仅按分段归组会把它们混入 server 技能表。键为 fullname 首段
/// 小写（与 host 侧 `mcp_source_key` 同源）。
fn group_mcp_skills(entries: &[SlashCommandEntry]) -> HashMap<String, Vec<SkillEntry>> {
    let mut map: HashMap<String, Vec<SkillEntry>> = HashMap::new();
    for entry in entries {
        if entry.kind != SlashActionKind::McpSkill {
            continue;
        }
        if let Some((server, _)) = entry.fullname.split_once(':') {
            map.entry(server.to_lowercase())
                .or_default()
                .push(SkillEntry {
                    name: entry.fullname.clone(),
                    description: entry.description.clone(),
                });
        }
    }
    map
}

#[component]
pub fn McpPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let control = hooks.use_state(actions::ControlState::default);
    let control_scope = actions::current_scope();
    let scope_for_effect = control_scope.clone();
    hooks.use_effect(
        move || control.write().reset_scope(scope_for_effect.clone()),
        (control_scope.clone(),),
    );
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
    // 每 server 已注入的 MCP skill 命令（决策 1：命令面 fullname =
    // `{server}:{skill}`，来自 available_commands_update 投影；discovery
    // 完成 → 注册表 on_change → 广播 → 本 atom 自动刷新，无需轮询）。
    // 命令面首段 = server 名末段小写（mcp_source_key 派生，与 host 侧
    // 同源），查询时按同规则归一。
    let cmd_store = hooks.use_atom(&AVAILABLE_SLASH_COMMANDS);
    // 每 server 的 MCP skill 命令（决策 1：命令面 fullname = `{server}:{skill}`）。
    // 键为 fullname 首段小写（命令面的 server 域名），供列表计数与详情技能表共用。
    let skills_by_server = group_mcp_skills(cmd_store.read().as_slice());
    let _ = cmd_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    // 视图状态：List ⇄ Detail（OAuth 授权入口）
    let view = hooks.use_state(|| McpView::List);
    // 详情目标身份：server 名（稳定 key）。进入详情时固定，返回列表后清除；
    // 不用列表下标，刷新缩短列表不会把详情切到别的 server。
    let detail_key = hooks.use_state(|| Option::<String>::None);
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
    // 列表视口：绘制与鼠标命中共用同一份结果，面板高度变化时可见项数随之变化
    let (_term_w, term_h) = hooks.use_terminal_size();
    let viewport = ListViewport::for_content_rows(
        content_rows(area, term_h),
        LIST_METRICS,
        servers.len(),
        *selected.read(),
    );
    // 详情目标消失（列表刷新）在 effect 边界收敛：渲染只读状态，不写状态
    let detail_missing =
        *view.read() == McpView::Detail && detail_server(&servers, &detail_key.read()).is_none();
    hooks.use_effect(
        move || {
            *detail_key.write() = None;
            *detail_btn.write() = 0;
            *sv.write() = ScrollViewState::default();
            *view.write() = McpView::List;
        },
        (detail_missing,),
    );

    // 事件闭包另持一份 servers / 技能快照（与渲染端共用同一 atom 副本；详情
    // 鼠标命中需要与渲染相同的技能行数）。
    let servers_for_closure = servers.clone();
    let skills_for_closure = skills_by_server.clone();

    hooks.use_event_handler_with_options(
        EventScope::Current,
        // High：详情视图的 Esc（返回列表）必须先于根层 Normal Esc（关面板）
        // 被消费——同优先级先注册先消费，Normal 会变成死代码（rewind_popup 同款坑）。
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            handle_mcp_event(
                event,
                &McpEventCtx {
                    area,
                    view,
                    selected,
                    detail_key,
                    detail_btn,
                    sv,
                    control,
                    control_scope: control_scope.clone(),
                    viewport,
                    servers: &servers_for_closure,
                    skills_by_server: &skills_for_closure,
                },
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        view,
        selected,
        detail_key,
        detail_btn,
        sv,
        control,
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    if *view.read() == McpView::Detail {
        // ── 详情视图（OAuth 授权入口）──
        if let Some(summary) = detail_server(&servers, &detail_key.read()) {
            let skills = skills_by_server
                .get(&server_domain_key(&summary.name))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            lines = build_detail_lines(
                summary,
                skills,
                *detail_btn.read(),
                &theme_def.read(),
                content_width,
            );
            if !summary.control.restart && !summary.control.stop {
                lines.push(
                    Line::from(actions::unavailable_message(
                        summary.control.unavailable_reason.as_deref(),
                    ))
                    .fg(theme_def.read().semantic.text.muted),
                );
            }
            if let Some(message) = control.read().message(&summary.name) {
                lines.push(Line::from(message).fg(theme_def.read().semantic.status.warning));
            }
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
            for (i, s) in viewport.window(&servers) {
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
                                        .get(&server_domain_key(&s.name))
                                        .map_or(0, Vec::len)
                                        as i64,
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
    // 内容行数即滚动缓冲区高度：Paragraph 不换行（长行由面板裁切），每行恰占
    // 1 个视觉行。必须显式传给 `View`——ScrollView 按子节点约束推导内容高度，
    // 而 `Text` 默认 `Constraint::Percentage(100)` 会与视口等高，内容永不溢出，
    // 渲染期把外部滚动偏移钳回 0（滚轮因此完全失效）。
    let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入；区域与列表命中同源，首帧未测量时为 0 尺寸）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Mcp, area.unwrap_or_default(), sv);

    panel_shell!(PanelKind::Mcp, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                state: Some(sv),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                View(
                    width: Constraint::Fill(1),
                    height: Constraint::Length(content_height),
                ) {
                    Text(text: content)
                }
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
fn handle_mcp_event(event: Event, ctx: &McpEventCtx<'_>) -> EventResult {
    if popup_owns_foreground() {
        return EventResult::Ignored;
    }

    // ── 鼠标 ──
    if let Event::Mouse(mouse) = &event {
        let Some((row, col)) = left_down(mouse) else {
            return EventResult::Ignored;
        };
        let Some(area) = ctx.area else {
            return EventResult::Consumed;
        };

        if *ctx.view.read() != McpView::Detail {
            // 列表视图：区域内左键点击 = 选中该项并执行 Enter（click as enter）。
            // 命中布局与绘制窗口同源（viewport）。
            if !is_scrollbar_column(mouse, area)
                && let Some(idx) = hit_item(mouse, area, ctx.viewport.hit_layout())
                && let Some(summary) = ctx.servers.get(idx)
            {
                *ctx.selected.write() = idx;
                *ctx.detail_key.write() = Some(summary.name.clone());
                *ctx.detail_btn.write() = default_detail_btn(summary);
                // 详情总是从顶部开始：列表的滚动偏移不得带入详情（否则按钮
                // 命中行与可见行错位）。
                *ctx.sv.write() = ScrollViewState::default();
                *ctx.view.write() = McpView::Detail;
            }
            return EventResult::Consumed;
        }

        // 详情视图：按钮行左键点击 = 激活按钮
        // 顶部边框行不可点
        if row < area.y.saturating_add(1) {
            return EventResult::Consumed;
        }
        // 内容行号 = 屏幕行 − 边框 + 滚动偏移：ScrollView 滚动后，可见的首行
        // 对应内容区第 offset 行。不加偏移会把滚动后的按钮判成未滚动行，
        // 导致点击失效/误触 header 复制按钮。
        let content_row = row
            .saturating_sub(area.y)
            .saturating_sub(1)
            .saturating_add(ctx.sv.read().offset().y);
        let Some(summary) = detail_server(ctx.servers, &ctx.detail_key.read()) else {
            return EventResult::Consumed;
        };
        // 技能行数参与表格行数推导：与渲染使用同一份快照。
        let skills_count = ctx
            .skills_by_server
            .get(&server_domain_key(&summary.name))
            .map_or(0, Vec::len);
        // 按钮命中：行号与列偏移取自渲染同源的 detail_buttons（header 右对齐
        // 复制按钮与底行按钮），列按显示宽度计算（CJK label 双宽）
        for button in detail_buttons(summary, skills_count, area.width) {
            if content_row != button.row {
                continue;
            }
            let x = area.x.saturating_add(button.col);
            if col >= x && col < x.saturating_add(button.hit_width) {
                activate_detail_btn(
                    button.sel,
                    summary,
                    &ctx.view,
                    &ctx.detail_btn,
                    &ctx.sv,
                    ctx.control,
                    &ctx.control_scope,
                );
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

    if *ctx.view.read() == McpView::Detail {
        // ── 详情视图：←→ 选按钮，Enter 激活，Esc 返回列表 ──
        match key.code {
            KeyCode::Esc => {
                let target = ctx.detail_key.read().clone();
                if target
                    .as_deref()
                    .is_some_and(|name| ctx.control.write().cancel_wait(name))
                {
                    notify(i18n::tr("panel-mcp-control-unknown"));
                    return EventResult::Consumed;
                }
                *ctx.detail_key.write() = None;
                *ctx.sv.write() = ScrollViewState::default();
                *ctx.view.write() = McpView::List;
            }
            KeyCode::Left | KeyCode::Right => {
                let Some(summary) = detail_server(ctx.servers, &ctx.detail_key.read()) else {
                    return EventResult::Consumed;
                };
                // 选择索引空间 = detail_actions（header 复制 → 底行 授权/返回）；
                // 只有一个按钮（[ 返回 ]）时无选择余地。
                let count = detail_actions(summary).len();
                if count > 1 {
                    let mut b = ctx.detail_btn.write();
                    *b = if key.code == KeyCode::Right {
                        (*b + 1) % count
                    } else {
                        (*b + count - 1) % count
                    };
                }
            }
            KeyCode::Enter => {
                let b = *ctx.detail_btn.read();
                if let Some(summary) = detail_server(ctx.servers, &ctx.detail_key.read()) {
                    activate_detail_btn(
                        b,
                        summary,
                        &ctx.view,
                        &ctx.detail_btn,
                        &ctx.sv,
                        ctx.control,
                        &ctx.control_scope,
                    );
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
            let sel = *ctx.selected.read();
            if let Some(summary) = ctx.servers.get(sel) {
                *ctx.detail_key.write() = Some(summary.name.clone());
                *ctx.detail_btn.write() = default_detail_btn(summary);
                *ctx.sv.write() = ScrollViewState::default();
                *ctx.view.write() = McpView::Detail;
            }
        }
        KeyCode::Up => {
            let mut s = ctx.selected.write();
            *s = previous_selection(*s);
        }
        KeyCode::Down => {
            let mut s = ctx.selected.write();
            let count = MCP_SERVERS.state().read().len();
            if count > 0 {
                *s = next_selection(*s, count);
            }
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 事件处理上下文：本帧视口 + 面板句柄 + 列表快照。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口
/// 签名不再随状态/视口增长（渲染与鼠标命中必须共用同一个 `viewport`）。
struct McpEventCtx<'a> {
    area: Option<Rect>,
    view: State<McpView>,
    selected: State<usize>,
    detail_key: State<Option<String>>,
    detail_btn: State<usize>,
    /// 面板滚动状态：详情鼠标命中必须加上 `offset().y`（渲染在 ScrollView 内）。
    sv: State<ScrollViewState>,
    control: State<actions::ControlState>,
    control_scope: crate::application::operations::OperationScope,
    viewport: ListViewport,
    servers: &'a [McpServerSummary],
    /// 当前命令面技能快照（详情鼠标命中按 server 域名取行数）。
    skills_by_server: &'a HashMap<String, Vec<SkillEntry>>,
}

/// 激活详情视图按钮：索引对应 `detail_actions`（header 复制 → 底行 授权/返回）。
/// 键盘 Enter / 鼠标左键点击共用。复制留在详情视图（错误原文仍需对照阅读）；
/// 授权/返回回到列表，并重置滚动偏移（列表总是从顶部开始）。
fn activate_detail_btn(
    idx: usize,
    summary: &McpServerSummary,
    view: &ReactiveHandle<McpView, SingleWaker>,
    detail_btn: &ReactiveHandle<usize, SingleWaker>,
    sv: &ReactiveHandle<ScrollViewState, SingleWaker>,
    control: State<actions::ControlState>,
    scope: &crate::application::operations::OperationScope,
) {
    match detail_action_at(summary, idx) {
        Some(DetailAction::CopyError) => copy_error_summary(summary),
        Some(DetailAction::Restart) => actions::dispatch(
            &summary.name,
            actions::ControlAction::Restart,
            scope.clone(),
            control,
        ),
        Some(DetailAction::Stop) => actions::dispatch(
            &summary.name,
            actions::ControlAction::Stop,
            scope.clone(),
            control,
        ),
        Some(DetailAction::Auth) => {
            start_oauth(summary.name.clone());
            *detail_btn.write() = 0;
            *sv.write() = ScrollViewState::default();
            *view.write() = McpView::List;
        }
        // 索引失效（详情打开期间列表被刷新）等同 [ 返回 ]
        Some(DetailAction::Back) | None => {
            *detail_btn.write() = 0;
            *sv.write() = ScrollViewState::default();
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
#[path = "mcp/detail_tables_test.rs"]
mod detail_tables_test;

#[cfg(test)]
#[path = "mcp/scroll_test.rs"]
mod scroll_test;

#[cfg(test)]
mod tests;
