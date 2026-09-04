//! ratatui-kit SubAgentDetailPanel component.
//!
//! §6.7 subagent 详情 pane：Enter 打开 nested transcript 或详情 pane，不把
//! 完整嵌套消息铺入主时间轴。本面板从 VIEW_MODELS 扫描 `TuiSubAgentGroup`
//! （按 `SELECTED_SUBAGENT_ID` 匹配，由消息区焦点分派在 Enter 时写入），
//! 用 `GridSpec::with_content` 嵌套渲染子消息，复用 `vm_to_lines_cached`
//! （render_copy_button=false——嵌套渲染不渲染 md 复制按钮，与历史
//! SubAgentGroup 递归渲染口径一致）。
//!
//! 只读面板 + 可滚动（`register_panel_scroll` 面板滚轮仲裁）；Esc 单层关闭
//! （栈顶弹栈，不触及其他面板/焦点仲裁）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    BG_DISPLAY, BG_LIVE_DETAIL, BgDisplayEntry, LANG_VERSION, SELECTED_SUBAGENT_ID, VIEW_MODELS,
};
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render::vm_to_lines_cached;
use crate::kit::panel_registry::clean_scrollbars;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldTarget, TuiRenderUnit, TuiSubAgentGroup, fold_for_status,
};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn SubAgentDetailPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);

    // 选中 subagent：SELECTED_SUBAGENT_ID（消息区焦点分派写入）→ VIEW_MODELS 扫描
    let selected_id = SELECTED_SUBAGENT_ID.state().read().clone();
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let display_store = hooks.use_atom(&BG_DISPLAY);
    let live_store = hooks.use_atom(&BG_LIVE_DETAIL);
    let group = find_selected_subagent(&vm_store.read(), selected_id.as_deref()).or_else(|| {
        find_live_detail_subagent(
            &live_store.read(),
            &display_store.read(),
            selected_id.as_deref(),
        )
    });
    let _ = vm_store;
    let _ = display_store;
    let _ = live_store;

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        // Esc 单层关闭（面板打开时 active_layer=Panel，消息区 Esc 分支放行，
        // 这里栈顶弹栈收尾；嵌套消息不参与消息区焦点仲裁）。
        if key.code == KeyCode::Esc {
            close_panel();
        }
        EventResult::Consumed
    });

    // 面板绘制区域（上一帧）——嵌套渲染 wrap 宽度 + 滚轮仲裁
    let area = hooks.use_previous_size();
    let grid = GridSpec::with_content(area.width.saturating_sub(2).max(1));

    // 嵌套渲染：组内 view_models → vm_to_lines_cached（复用统一水平网格与
    // markdown 增量缓存；md 复制按钮关闭——嵌套内容不提供复制按钮）。
    let mut lines: Vec<Line<'static>> = Vec::new();
    match &group {
        Some(g) => {
            lines.push(Line::from(vec![Span::styled(
                g.agent_name.clone(),
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold(),
            )]));
            lines.push(Line::from(vec![Span::styled(
                format!("  [{}]", g.agent_id),
                Style::new().fg(theme_def.read().semantic.text.dim),
            )]));
            lines.push(Line::from(""));
            let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
            for vm in g.view_models.iter() {
                let (vm_lines, _, _, _) = vm_to_lines_cached(vm, &grid, &mut cache, false);
                lines.extend(vm_lines);
            }
        }
        None => {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("subagent-detail-not-found"),
                Style::new().fg(theme_def.read().semantic.text.muted),
            )]));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    crate::kit::panel_scroll::register_panel_scroll(PanelKind::SubAgentDetail, area, sv);

    panel_shell!(PanelKind::SubAgentDetail, {
        ScrollView(
            scrollbars: clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}

/// 从 VIEW_MODELS 快照扫描 `TuiSubAgentGroup`，按 agent_id 匹配选中项。
/// 扫描口径与 agent.rs `collect_subagents` 一致（含折叠组内嵌套递归）。
fn find_selected_subagent(
    snap: &crate::kit::atoms::ViewModelsSnapshot,
    selected_id: Option<&str>,
) -> Option<TuiSubAgentGroup> {
    let selected_id = selected_id?;
    for vm in snap.items.iter() {
        if let Some(g) = scan_vm_for_subagent(vm, selected_id) {
            return Some(g);
        }
    }
    None
}

/// 递归扫描单个 VM（含 TuiCollapsedGroup 内层与嵌套 SubAgent 内层）。
fn scan_vm_for_subagent(vm: &TuiRenderUnit, selected_id: &str) -> Option<TuiSubAgentGroup> {
    match vm {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            if g.instance_id == selected_id || g.agent_id == selected_id {
                Some(g.clone())
            } else {
                // 嵌套 SubAgent 内层也扫描（agent.rs 同口径）
                g.view_models
                    .iter()
                    .find_map(|inner| scan_vm_for_subagent(inner, selected_id))
            }
        }
        TuiRenderUnit::TuiCollapsedGroup(g) => g
            .view_models
            .iter()
            .find_map(|inner| scan_vm_for_subagent(inner, selected_id)),
        _ => None,
    }
}

fn find_live_detail_subagent(
    live: &std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    display: &[BgDisplayEntry],
    selected_id: Option<&str>,
) -> Option<TuiSubAgentGroup> {
    let selected_id = selected_id?;
    let task_id = if live.contains_key(selected_id) {
        selected_id
    } else {
        display
            .iter()
            .rev()
            .find(|entry| entry.linked_agent_id.as_deref() == Some(selected_id))?
            .id
            .as_str()
    };
    let detail = live.get(task_id)?;
    let status = if detail.subagent_is_error {
        EntryStatus::Error
    } else if detail.status == crate::kit::atoms::BgLiveStatus::Running {
        EntryStatus::Running
    } else {
        EntryStatus::Completed
    };
    let mut group = TuiSubAgentGroup {
        instance_id: task_id.to_string(),
        agent_id: detail
            .agent_id
            .clone()
            .unwrap_or_else(|| selected_id.to_string()),
        agent_name: detail
            .agent_name
            .clone()
            .unwrap_or_else(|| detail.summary.clone()),
        view_models: detail.nested_units.clone(),
        collapsed: false,
        is_running: detail.status == crate::kit::atoms::BgLiveStatus::Running,
        is_error: detail.subagent_is_error,
        error_reason: detail.subagent_result.clone(),
        fold: fold_for_status(FoldTarget::SubAgent, status),
        user_modified: false,
        content_hash: 0,
    };
    group.recompute_hash();
    Some(group)
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "subagent_detail_test.rs"]
mod tests;
