//! ratatui-kit StatusPanel component.
//!
//! S6c：双 Tab（Service / Context）——Service Tab 直接从 `SERVICE_SNAPSHOT` atom
//! 读 CPU/MEM/provider/model/permission_mode/cron 统计，**无需任何 mock**。
//! Context Tab 暂显示占位（context token 计数需要 S11 解耦后从 ACP 流接入）。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::kit::atoms::{SERVICE_SNAPSHOT, VIEW_MODELS};
use crate::kit::theme;
use peri_acp_types::view_model::ViewModel;

const TAB_SERVICE: usize = 0;
const TAB_CONTEXT: usize = 1;

#[component]
pub fn StatusPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let active_tab = hooks.use_state(|| TAB_SERVICE);

    // S6c: 订阅 SERVICE_SNAPSHOT——后台 service_snapshot 2s 派生一次
    let snapshot_store = hooks.use_store(*SERVICE_SNAPSHOT.get().unwrap());
    let snap = snapshot_store.read().clone();
    let _ = snapshot_store; // StoreState 是 Copy，无需显式 drop

    // H1a: 订阅 VIEW_MODELS，从派生 Context Tab 的消息计数（committed + current_turn
    // 的 ViewModel 分类统计）。这避免了占位文本，让 Context Tab 反映真实状态。
    let vm_store = hooks.use_store(*VIEW_MODELS.get().unwrap());
    let vm_stats = derive_vm_stats(&vm_store.read());
    let _ = vm_store;

    hooks.use_local_events({
        let active_tab = active_tab;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => close_panel(),
                    KeyCode::Left => {
                        *active_tab.write() = TAB_SERVICE;
                    }
                    KeyCode::Right => {
                        *active_tab.write() = TAB_CONTEXT;
                    }
                    // Esc 由 PanelOverlay 上层处理
                    _ => {}
                }
            }
        }
    });

    let tab = *active_tab.read();

    // ── Tab bar ──────────────────────────────────────────────────────
    let tab_bar = Paragraph::new(Line::from(vec![
        Span::styled(
            " Service ",
            if tab == TAB_SERVICE {
                Style::new().fg(theme::TEXT).bg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
        Span::styled(
            " Context ",
            if tab == TAB_CONTEXT {
                Style::new().fg(theme::TEXT).bg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
    ]));

    // ── Content ──────────────────────────────────────────────────────
    let provider_label = if snap.provider_name.is_empty() {
        "(unconfigured)".to_string()
    } else {
        snap.provider_name.clone()
    };
    let model_label = if snap.model_alias.is_empty() {
        "(none)".to_string()
    } else {
        snap.model_alias.clone()
    };
    let mode_label = if snap.permission_mode.is_empty() {
        "default".to_string()
    } else {
        snap.permission_mode.clone()
    };
    let mcp_label = format!("{}/{} connected", snap.mcp.connected, snap.mcp.total);
    let mcp_phase = match snap.mcp.init_phase {
        crate::kit::atoms::McpInitPhase::Pending => "pending",
        crate::kit::atoms::McpInitPhase::Initializing => "initializing",
        crate::kit::atoms::McpInitPhase::Ready => "ready",
        crate::kit::atoms::McpInitPhase::Failed => "failed",
    };

    let content_lines: Vec<Line<'_>> = match tab {
        TAB_SERVICE => vec![
            Line::from(vec![
                Span::styled("Provider:   ", Style::new().fg(theme::MUTED)),
                Span::styled(provider_label, Style::new().fg(theme::TEXT).bold()),
            ]),
            Line::from(vec![
                Span::styled("Model:      ", Style::new().fg(theme::MUTED)),
                Span::styled(model_label, Style::new().fg(theme::TEXT).bold()),
            ]),
            Line::from(vec![
                Span::styled("Permission: ", Style::new().fg(theme::MUTED)),
                Span::styled(mode_label, Style::new().fg(theme::ACCENT).bold()),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("CPU:        ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{:.1}%", snap.cpu_percent),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("Memory:     ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{} MB", snap.memory_mb),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("MCP:        ", Style::new().fg(theme::MUTED)),
                Span::styled(mcp_label, Style::new().fg(theme::SAGE)),
                Span::styled(format!("  [{}]", mcp_phase), Style::new().fg(theme::MUTED)),
            ]),
            Line::from(vec![
                Span::styled("Cron:       ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{} ({} enabled)", snap.cron_total, snap.cron_enabled),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("cwd: ", Style::new().fg(theme::MUTED)),
                Span::styled(snap.cwd.clone(), Style::new().fg(theme::TEXT)),
            ]),
        ],
        TAB_CONTEXT => vec![
            Line::from(vec![
                Span::styled("Total VMs:        ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{}", vm_stats.total),
                    Style::new().fg(theme::TEXT).bold(),
                ),
            ]),
            Line::from(vec![
                Span::styled("  User turns:     ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{}", vm_stats.user_bubbles),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Assistant turns:", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{}", vm_stats.assistant_bubbles),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Tool calls:     ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{}", vm_stats.tool_cards),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("  SubAgent groups:", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{}", vm_stats.subagent_groups),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("  System notes:   ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    format!("{}", vm_stats.system_notes),
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "  Token-level budget requires ACP stream; VM counts shown here are derived locally.",
                Style::new().fg(theme::DIM).italic(),
            )]),
        ],
        _ => vec![Line::from("  Unknown tab").fg(theme::MUTED)],
    };

    // ── Footer ───────────────────────────────────────────────────────
    let footer = Line::from("  ← →) Switch Tab  Esc) Close").fg(theme::DIM);

    let content = Paragraph::new(ratatui::text::Text::from({
        let mut all: Vec<Line> = Vec::new();
        all.push(Line::from("")); // spacer after tab bar
        all.extend(content_lines);
        all.push(Line::from(""));
        all.push(footer);
        all
    }));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Status ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(48),
            height: Constraint::Length(16),
        ) {
            Text(text: tab_bar)
            Text(text: content)
        }
    )
}

/// H1a：从 ViewModelsSnapshot 派生按 ViewModel 类型分类的统计。
struct VmStats {
    total: usize,
    user_bubbles: usize,
    assistant_bubbles: usize,
    tool_cards: usize,
    subagent_groups: usize,
    system_notes: usize,
}

fn derive_vm_stats(snap: &crate::kit::atoms::ViewModelsSnapshot) -> VmStats {
    let mut s = VmStats {
        total: 0,
        user_bubbles: 0,
        assistant_bubbles: 0,
        tool_cards: 0,
        subagent_groups: 0,
        system_notes: 0,
    };
    for vm in snap.committed.iter().chain(snap.current_turn.iter()) {
        count_vm(vm, &mut s);
    }
    s.total =
        s.user_bubbles + s.assistant_bubbles + s.tool_cards + s.subagent_groups + s.system_notes;
    s
}

fn count_vm(vm: &ViewModel, s: &mut VmStats) {
    match vm {
        ViewModel::UserBubble(_) => s.user_bubbles += 1,
        ViewModel::AssistantBubble(_) => s.assistant_bubbles += 1,
        ViewModel::ToolCard(_) => s.tool_cards += 1,
        ViewModel::SubAgentGroup(g) => {
            s.subagent_groups += 1;
            for child in g.view_models.iter() {
                count_vm(child, s);
            }
        }
        ViewModel::SystemNote(_) => s.system_notes += 1,
        ViewModel::CollapsedGroup(g) => {
            for child in g.view_models.iter() {
                count_vm(child, s);
            }
        }
        _ => {}
    }
}

fn close_panel() {
    use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
    if let Some(atom) = ACTIVE_PANEL.get() {
        *atom.write() = None;
    }
    if let Some(atom) = OPEN_PANELS.get() {
        atom.write().clear();
    }
}
