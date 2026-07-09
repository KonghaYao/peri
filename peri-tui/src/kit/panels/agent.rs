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

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT, VIEW_MODELS};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
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
pub fn AgentPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

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

    let subagent_count = subagents.len();

    // 候选行数（仅用于 cursor 边界）
    let row_count = 8 + subagent_count.max(1);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
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
                    let mut c = cursor.write();
                    *c = previous_selection(*c);
                }
                KeyCode::Down => {
                    let mut c = cursor.write();
                    if row_count > 0 {
                        *c = next_selection(*c, row_count);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    // 从 PERI_CONFIG_HANDLE 派生 provider_id 和 active alias
    let (active_provider_id, active_alias) = PERI_CONFIG_HANDLE
        .get()
        .map(|h| {
            let cfg = h.read();
            (
                cfg.config.active_provider_id.clone(),
                cfg.config.active_alias.clone(),
            )
        })
        .unwrap_or_else(|| ("?".to_string(), "?".to_string()));

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 头部
    lines.push(Line::from(vec![Span::styled(
        "  Current Agent Session",
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  ----------------------",
        Style::new().fg(theme::semantic().text.dim),
    )]));
    lines.push(Line::from(""));

    // 元信息行
    let meta_rows: Vec<(&str, String)> = vec![
        (
            "Provider",
            format!("{} ({})", provider_name, active_provider_id),
        ),
        (
            "Model",
            format!("{} (alias: {})", model_alias, active_alias),
        ),
        ("Permission Mode", permission_mode),
        ("CWD", cwd),
        ("Messages", format!("Messages: {total_messages}",)),
        ("Total Messages", format!("{}", total_messages)),
    ];

    for (i, (label, value)) in meta_rows.iter().enumerate() {
        let is_selected = i == sel;
        let cursor_mark = if is_selected { ">" } else { " " };
        let label_style = if is_selected {
            Style::new().fg(theme::component().panel.title).bold()
        } else {
            Style::new().fg(theme::semantic().text.muted)
        };
        let value_style = if is_selected {
            Style::new().fg(theme::semantic().text.primary).bold()
        } else {
            Style::new().fg(theme::semantic().text.primary)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::component().panel.title),
            ),
            Span::styled(format!("{:<18}", format!("{}:", label)), label_style),
            Span::styled(value.chars().take(60).collect::<String>(), value_style),
        ]));
    }

    // SubAgent 列表标题
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  SubAgents ({})", subagent_count),
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));

    if subagents.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No sub-agents spawned in this session",
            Style::new().fg(theme::semantic().text.muted).italic(),
        )]));
    } else {
        for (i, sa) in subagents.iter().enumerate() {
            let row_idx = meta_rows.len() + 1 + i;
            let is_selected = row_idx == sel;
            let cursor_mark = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };
            let status_marker = if sa.collapsed {
                Span::styled(
                    " (collapsed)",
                    Style::new().fg(theme::semantic().text.muted),
                )
            } else {
                Span::styled(
                    " (expanded)",
                    Style::new().fg(theme::semantic().status.success),
                )
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_mark),
                    Style::new().fg(theme::component().panel.title),
                ),
                Span::styled(sa.agent_name.clone(), name_style),
                Span::styled(
                    format!("  [{}]", sa.agent_id),
                    Style::new().fg(theme::semantic().text.dim),
                ),
                status_marker,
                Span::styled(
                    format!("  {} msgs", sa.view_models.len()),
                    Style::new().fg(theme::semantic().text.muted),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme::semantic().text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Agent, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}

/// 从 ViewModelsSnapshot 派生 SubAgent 列表（按出现顺序，去重）。
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
mod tests {
    use super::*;
    use crate::kit::atoms::ViewModelsSnapshot;
    use crate::kit::tui_render_unit::{TuiCollapsedGroup, TuiSubAgentGroup, TuiUserBubble};

    fn make_subagent(id: &str, name: &str) -> TuiSubAgentGroup {
        TuiSubAgentGroup {
            agent_id: id.to_string(),
            agent_name: name.to_string(),
            view_models: im::Vector::new(),
            collapsed: false,
            is_running: false,
            content_hash: 0,
        }
    }

    #[test]
    fn test_collect_subagents_empty_snapshot() {
        let snap = ViewModelsSnapshot::default();
        assert!(collect_subagents(&snap).is_empty());
    }

    #[test]
    fn test_collect_subagents_only_user_bubbles() {
        let snap = ViewModelsSnapshot {
            items: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "hi".to_string(),
                content_hash: 0,
                reminder: None,
            })]),
            generation: 0,
        };
        assert!(collect_subagents(&snap).is_empty());
    }

    #[test]
    fn test_collect_subagents_dedup_across_committed_and_current() {
        // 同一 agent_id 出现在 items 中两次——应只保留一次
        let snap = ViewModelsSnapshot {
            items: im::Vector::from(vec![
                TuiRenderUnit::TuiSubAgentGroup(make_subagent("researcher", "Researcher")),
                TuiRenderUnit::TuiSubAgentGroup(make_subagent("researcher", "Researcher")),
            ]),
            generation: 0,
        };
        let result = collect_subagents(&snap);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].agent_id, "researcher");
    }

    #[test]
    fn test_collect_subagents_preserves_insertion_order() {
        let snap = ViewModelsSnapshot {
            items: im::Vector::from(vec![
                TuiRenderUnit::TuiSubAgentGroup(make_subagent("alpha", "Alpha")),
                TuiRenderUnit::TuiSubAgentGroup(make_subagent("beta", "Beta")),
                TuiRenderUnit::TuiSubAgentGroup(make_subagent("gamma", "Gamma")),
            ]),
            generation: 0,
        };
        let result = collect_subagents(&snap);
        let ids: Vec<_> = result.iter().map(|s| s.agent_id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn test_collect_subagents_recurses_into_collapsed_group() {
        // TuiCollapsedGroup 内嵌 SubAgent——应被扫描到
        let collapsed = TuiCollapsedGroup {
            title: "batch".to_string(),
            count: 1,
            view_models: vec![TuiRenderUnit::TuiSubAgentGroup(make_subagent(
                "hidden", "Hidden",
            ))],
            content_hash: 0,
        };
        let snap = ViewModelsSnapshot {
            items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(collapsed)]),
            generation: 0,
        };
        let result = collect_subagents(&snap);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].agent_id, "hidden");
    }

    #[test]
    fn test_collect_subagents_recurses_into_nested_subagent() {
        // SubAgent 内嵌 SubAgent（嵌套）——内层也应被扫描
        let mut outer = make_subagent("outer", "Outer");
        let mut outer_vms: Vec<TuiRenderUnit> = Vec::new();
        outer_vms.push(TuiRenderUnit::TuiSubAgentGroup(make_subagent(
            "inner", "Inner",
        )));
        outer.view_models = im::Vector::from(outer_vms);
        let snap = ViewModelsSnapshot {
            items: im::Vector::from(vec![TuiRenderUnit::TuiSubAgentGroup(outer)]),
            generation: 0,
        };
        let result = collect_subagents(&snap);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].agent_id, "outer");
        assert_eq!(result[1].agent_id, "inner");
    }
}
