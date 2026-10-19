//! ratatui-kit AgentPanel component.
//!
//! H1e（Iteration 14）：从 SERVICE_SNAPSHOT + PERI_CONFIG_HANDLE + VIEW_MODELS
//! 派生当前 agent 会话的元信息（provider/model/permission_mode/cwd/subagent
//! 数量）。SubAgent 列表从 VIEW_MODELS 中扫描 `TuiSubAgentGroup` 变体派生——
//! 这是 v2 单路径架构下的权威数据源（子代理生命周期由 ACP 协议 + ViewCommit
//! 替换语义维护）。
//!
//! 只读面板——切换 provider/model 在 Login/Model 面板，permission_mode 在
//! Config 面板。
//!
//! hook 顺序（TUI-HOOK-001）：`hooks.use_*` 无条件排在函数体前部，条件分支与提前
//! `return` 一律排在其后——某条分支少调一个 hook，框架就会按下标取到错位的 hook
//! 而 panic（ratatui-kit `Hooks::use_hook`）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT, VIEW_MODELS};
use crate::kit::list_nav::{clamp_selection, next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column, left_down};
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::{Paragraph, Wrap},
    },
};

/// 元信息行条数——cursor 索引空间的前半段（`0..META_ROWS`），其后紧接 SubAgent 行。
/// `meta_rows` 数组字面量的长度由该常量约束，索引空间不会与渲染行数各算一份。
const META_ROWS: usize = 6;

/// 内容区测量宽度扣除量——仅在 ScrollView 尚未渲染（首帧、无法给出实测视口宽度）时兜底。
const PANEL_CONTENT_INSET: u16 = 2;

#[component]
pub fn AgentPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    // 上一帧的逐行实测命中区间——渲染体覆盖写入（render 派生缓存，
    // `write_no_update` 不触发重渲染：TUI-RENDER-001），鼠标事件按最近完成帧的
    // 几何命中（TUI-EVENT-001）。
    let hit_spans = hooks.use_state(Vec::<ItemSpan>::new);

    let snap_store = hooks.use_atom(&SERVICE_SNAPSHOT);
    let provider_name = snap_store.read().provider_name.clone();
    let model_alias = snap_store.read().model_alias.clone();
    let permission_mode = snap_store.read().permission_mode.clone();
    let cwd = snap_store.read().cwd.clone();
    let _ = snap_store;

    // 从 VIEW_MODELS 派生 subagent 列表 + 当前 iteration 计数
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let total_messages = vm_store.read().items.len();
    let subagents = collect_subagents(&vm_store.read());
    let _ = vm_store;

    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 面板绘制区域（上一帧实测）——滚轮仲裁注册与鼠标命中共用这一份来源；首帧未测量
    // 时为 `None`（命中放弃、滚动注册退化为 0 尺寸），下一帧起生效。
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // cursor 索引空间：元信息行在前，SubAgent 行紧随其后（连续，无不可选空洞），
    // 与渲染时的选中判定同源。
    let item_count = META_ROWS + subagents.len();

    // 列表刷新（切会话、子代理结束）缩短可选行时，越界光标在 effect 边界收回——
    // 渲染体只读状态（TUI-RENDER-001）。deps 只跟踪「是否越界」，正常帧不写状态。
    let cursor_overflow = *cursor.read() >= item_count;
    hooks.use_effect(
        move || {
            if *cursor.read() >= item_count {
                *cursor.write() = item_count.saturating_sub(1);
            }
        },
        (cursor_overflow,),
    );

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            handle_agent_event(
                event,
                &AgentEventCtx {
                    area,
                    cursor,
                    sv,
                    hit_spans,
                    item_count,
                },
            )
        },
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        cursor,
        sv,
        hit_spans,
    });

    // 从 PERI_CONFIG_HANDLE 派生 provider_id（active profile 携带）和 active alias
    let (active_provider_id, active_alias) = PERI_CONFIG_HANDLE
        .get()
        .map(|h| {
            let cfg = h.read();
            (
                cfg.config
                    .profiles
                    .get(&cfg.config.active_alias)
                    .map(|p| p.provider.clone())
                    .unwrap_or_default(),
                cfg.config.active_alias.clone(),
            )
        })
        .unwrap_or_else(|| ("?".to_string(), "?".to_string()));

    let sel = *cursor.read();

    // 内容宽度：优先用 ScrollView 上一帧实测视口——它已是子节点的真实排版宽度（滚动条
    // 占不占列由它决定），换行高度与命中行号都按它算，绘制与命中不会差一列；首帧尚未
    // 渲染时退回面板宽度扣除滚动条预留列。
    let content_width = sv
        .read()
        .page_size()
        .map(|size| size.width)
        .or_else(|| area.map(|rect| rect.width.saturating_sub(PANEL_CONTENT_INSET)))
        .unwrap_or_default()
        .max(1);

    // 逐行登记渲染内容：一次测量同时给出滚动缓冲高度与可选行的行区间。
    let mut body = PanelBody::new(content_width);

    // 头部
    body.push(Line::from(vec![Span::styled(
        i18n::tr("agent-panel-title-session"),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    body.push(Line::from(vec![Span::styled(
        "  ----------------------",
        Style::new().fg(theme_def.read().semantic.text.dim),
    )]));
    body.push(Line::from(""));

    // 元信息行
    let meta_rows: [(String, String); META_ROWS] = [
        (
            i18n::tr("agent-label-provider"),
            format!("{} ({})", provider_name, active_provider_id),
        ),
        (
            i18n::tr("agent-label-model"),
            format!("{} (alias: {})", model_alias, active_alias),
        ),
        (i18n::tr("agent-label-permission-mode"), permission_mode),
        (i18n::tr("agent-label-cwd"), cwd),
        (
            i18n::tr("agent-label-messages"),
            format!("{total_messages}"),
        ),
        (
            i18n::tr("agent-label-total-messages"),
            format!("{total_messages}"),
        ),
    ];

    for (i, (label, value)) in meta_rows.iter().enumerate() {
        let is_selected = i == sel;
        let cursor_mark = if is_selected { ">" } else { " " };
        let label_style = if is_selected {
            Style::new()
                .fg(theme_def.read().component.panel.title)
                .bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.muted)
        };
        let value_style = if is_selected {
            Style::new()
                .fg(theme_def.read().semantic.text.primary)
                .bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.primary)
        };

        body.push_item(
            i,
            Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_mark),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(format!("{:<18}", format!("{}:", label)), label_style),
                Span::styled(value.chars().take(60).collect::<String>(), value_style),
            ]),
        );
    }

    // SubAgent 列表标题
    body.push(Line::from(""));
    body.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "agent-subagents-count",
            &[(
                "count".to_string(),
                FluentValue::from(subagents.len() as i64),
            )],
        ),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));

    if subagents.is_empty() {
        body.push(Line::from(vec![Span::styled(
            i18n::tr("agent-no-subagents"),
            Style::new()
                .fg(theme_def.read().semantic.text.muted)
                .italic(),
        )]));
    } else {
        for (i, sa) in subagents.iter().enumerate() {
            let is_selected = META_ROWS + i == sel;
            let cursor_mark = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let status_marker = if sa.collapsed {
                Span::styled(
                    i18n::tr("agent-collapsed"),
                    Style::new().fg(theme_def.read().semantic.text.muted),
                )
            } else {
                Span::styled(
                    i18n::tr("agent-expanded"),
                    Style::new().fg(theme_def.read().semantic.status.success),
                )
            };
            body.push_item(
                META_ROWS + i,
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", cursor_mark),
                        Style::new().fg(theme_def.read().component.panel.title),
                    ),
                    Span::styled(sa.agent_name.clone(), name_style),
                    Span::styled(
                        format!("  [{}]", sa.agent_id),
                        Style::new().fg(theme_def.read().semantic.text.dim),
                    ),
                    status_marker,
                    Span::styled(
                        i18n::tr_args(
                            "agent-message-count",
                            &[(
                                "count".to_string(),
                                FluentValue::from(sa.view_models.len() as i64),
                            )],
                        ),
                        Style::new().fg(theme_def.read().semantic.text.muted),
                    ),
                ]),
            );
        }
    }

    body.push(Line::from(""));
    body.push(Line::from(i18n::tr("panel-agent-nav-hint")).fg(theme_def.read().semantic.text.dim));

    let content_height = body.height();
    let PanelBody { lines, spans, .. } = body;
    // 命中区间写回渲染派生缓存；未测量区域（首帧）时置空——那一帧的宽度只是兜底值，
    // 按它算出的行号会选中另一行，宁可不命中。
    *hit_spans.write_no_update() = if area.is_some() { spans } else { Vec::new() };

    let content = Paragraph::new(ratatui::text::Text::from(lines)).wrap(Wrap { trim: false });

    // 面板滚轮仲裁注册（每帧覆盖写入；区域与鼠标命中同源，均取自 `AreaTracker` 的
    // 上一帧实测区域，首帧未测量时为 0 尺寸，下一帧起生效）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Agent, area.unwrap_or_default(), sv);

    panel_shell!(PanelKind::Agent, {
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

/// 可选项在内容区占用的视觉行区间（内容行号，0 起）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ItemSpan {
    /// cursor 索引空间中的项号
    item: usize,
    /// 起始内容行号
    start: u16,
    /// 实测占用的视觉行数（长值换行后 > 1）
    rows: u16,
}

/// 渲染体：逐行登记，同时累计内容高度与可选行的实测行区间。
///
/// 绘制与命中共用同一次测量。Agent 面板行高可变（长 provider/cwd 值经
/// `Wrap{trim:false}` 折成多行），固定行高的 `ListMetrics` / `ListViewport` 表达不了，
/// 故不接入公共列表组件（`docs/design/tui-panel-architecture.md` 允许可变行高与无列表
/// 的只读面板不接入），改由这里给出实测区间。
struct PanelBody {
    lines: Vec<Line<'static>>,
    spans: Vec<ItemSpan>,
    width: u16,
    /// 已登记行的累计视觉行数（= 下一行的内容行号）
    row: u16,
}

impl PanelBody {
    fn new(width: u16) -> Self {
        Self {
            lines: Vec::new(),
            spans: Vec::new(),
            width,
            row: 0,
        }
    }

    /// 登记不可选行（标题、空行、段标题、空态占位、底部提示）。
    fn push(&mut self, line: Line<'static>) {
        self.row = self.row.saturating_add(line_height(&line, self.width));
        self.lines.push(line);
    }

    /// 登记可选行：记录它属于 cursor 索引空间的哪一项。
    fn push_item(&mut self, item: usize, line: Line<'static>) {
        let rows = line_height(&line, self.width);
        self.spans.push(ItemSpan {
            item,
            start: self.row,
            rows,
        });
        self.row = self.row.saturating_add(rows);
        self.lines.push(line);
    }

    /// 内容区总行数（滚动缓冲高度）。
    fn height(&self) -> u16 {
        self.row
    }
}

/// 内容行号 → 可选项索引（落在任何可选行区间之外时返回 `None`）。
fn hit_item_at(content_row: u16, spans: &[ItemSpan]) -> Option<usize> {
    spans
        .iter()
        .find(|span| content_row >= span.start && content_row - span.start < span.rows)
        .map(|span| span.item)
}

/// 面板事件处理——判定集中在这里，组件只做注册；测试经挂载探针取得句柄后直接投喂事件。
///
/// 鼠标只做「选中」：本面板没有任何 Enter 动作（Enter 与 Esc 一样关闭面板），
/// 点击不沿用 click-as-enter，避免点到列表就把面板关掉。
fn handle_agent_event(event: Event, ctx: &AgentEventCtx) -> EventResult {
    if let Event::Mouse(mouse) = &event {
        // 滚轮/拖拽让给面板滚轮仲裁（panel_scroll::register_panel_scroll）
        let Some((row, _col)) = left_down(mouse) else {
            return EventResult::Ignored;
        };
        let Some(area) = ctx.area else {
            return EventResult::Consumed;
        };
        // 顶部边框行与滚动条列不参与列表命中
        if row < area.y.saturating_add(1) || is_scrollbar_column(mouse, area) {
            return EventResult::Consumed;
        }
        let view_row = row.saturating_sub(area.y).saturating_sub(1);
        if view_row < area.height.saturating_sub(2) {
            // 命中区间用内容行号，滚过的偏移要加回来（视口第 0 行 = 内容第 offset 行）
            let content_row = view_row.saturating_add(ctx.sv.read().offset().y);
            if let Some(item) = hit_item_at(content_row, &ctx.hit_spans.read()[..]) {
                *ctx.cursor.write() = item;
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
    match key.code {
        KeyCode::Esc => close_panel(),
        KeyCode::Enter => close_panel(),
        KeyCode::Up => {
            let mut c = ctx.cursor.write();
            *c = clamp_selection(previous_selection(*c), ctx.item_count);
        }
        KeyCode::Down => {
            let mut c = ctx.cursor.write();
            *c = next_selection(clamp_selection(*c, ctx.item_count), ctx.item_count);
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 事件上下文：区域来源（上一帧实测）、命中区间（上一帧渲染实测）、
/// 滚动状态与 cursor 句柄，均由组件在注册时提供。
struct AgentEventCtx {
    area: Option<Rect>,
    cursor: State<usize>,
    sv: State<ScrollViewState>,
    hit_spans: State<Vec<ItemSpan>>,
    /// 本帧可选行数（元信息行 + SubAgent 行）——上下移动的边界
    item_count: usize,
}

/// 单个渲染行在 `width` 列下的实际行数（ratatui 逐行换行，故单行测量与整段一致）。
fn line_height(line: &Line<'_>, width: u16) -> u16 {
    wrapped_content_height(std::slice::from_ref(line), width)
}

/// 内容区扣除边框与滚动条后，一段文本在 `width` 列下的实际显示行数。
fn wrapped_content_height(lines: &[Line<'_>], width: u16) -> u16 {
    Paragraph::new(ratatui::text::Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
        .clamp(1, u16::MAX as usize) as u16
}

fn collect_subagents(snap: &crate::kit::atoms::ViewModelsSnapshot) -> Vec<TuiSubAgentGroup> {
    let mut out: Vec<TuiSubAgentGroup> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for vm in snap.items.iter() {
        scan_vm_for_subagents(vm, &mut out, &mut seen);
    }
    out
}

fn scan_vm_for_subagents(
    vm: &TuiRenderUnit,
    out: &mut Vec<TuiSubAgentGroup>,
    seen: &mut std::collections::HashSet<String>,
) {
    if let TuiRenderUnit::TuiSubAgentGroup(d) = vm {
        if seen.insert(d.agent_id.clone()) {
            out.push(d.clone());
        }
        // 递归扫描子 view_models（嵌套 TuiSubAgentGroup 罕见但支持）
        for child in d.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    } else if let TuiRenderUnit::TuiCollapsedGroup(g) = vm {
        for child in g.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;
