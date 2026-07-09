//! MessageArea：遍历 VIEW_MODELS atom，将全部 ViewModel 预计算为 Vec<Line<'static>>，
//! 缓存后以单个 Paragraph 渲染。避免每帧重建 N 个 ratatui-kit widget 树。
//!
//! - 滚动：由 ratatui-kit ScrollView 原生处理
//! - 智能跟随：loading 时自动滚到底
//! - Footer：spinner + todo items

#![allow(clippy::needless_update)]

use ratatui_kit::{
    components::ScrollViewState,
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::i18n;
use crate::kit::atoms::{self, LANG_VERSION};
use crate::kit::theme;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
use crate::kit::welcome::Welcome;

/// MessageArea 属性。
#[derive(Props, Default)]
pub struct MessageAreaProps {
    pub width: usize,
}

/// 渲染缓存：generation 不变时复用 Vec<Line>（Send + Sync，可存 use_state）。
#[derive(Clone)]
struct LinesCache {
    generation: u64,
    items_len: usize,
    is_loading: bool,
    todos_len: usize,
    lines: Vec<Line<'static>>,
}

#[component]
pub fn MessageArea(_props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let vm_snapshot = hooks.use_atom(&atoms::VIEW_MODELS);
    let acp_state = hooks.use_atom(&atoms::ACP_STATE);
    let todo_items = hooks.use_atom(&atoms::TODO_ITEMS);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let _heartbeat = hooks.use_atom(&atoms::RENDER_HEARTBEAT);

    let scroll_state = hooks.use_state(|| ScrollViewState::new());
    let cache = hooks.use_state(|| None::<LinesCache>);

    let snapshot = vm_snapshot.read();
    let is_loading = acp_state.read().is_loading;
    let todos = todo_items.read().clone();
    let has_content = !snapshot.items.is_empty();

    if !has_content && !is_loading {
        return element! { Welcome() }.into_any();
    }

    let todos_len = todos.len();
    let items_len = snapshot.items.len();
    let need_rebuild = match cache.read().as_ref() {
        None => true,
        Some(c) => {
            c.generation != snapshot.generation
                || c.is_loading != is_loading
                || c.todos_len != todos_len
                || c.items_len != items_len
        }
    };

    let all_lines: Vec<Line<'static>> = if need_rebuild {
        let mut lines = Vec::new();
        for vm in snapshot.items.iter() {
            render_vm_to_lines(vm, &mut lines);
        }
        build_footer_lines(is_loading, &todos, &mut lines);
        let mut c = cache.write();
        *c = Some(LinesCache {
            generation: snapshot.generation,
            items_len,
            is_loading,
            todos_len,
            lines: lines.clone(),
        });
        lines
    } else {
        cache.read().as_ref().unwrap().lines.clone()
    };

    // 智能跟随：loading 时滚到底
    if is_loading {
        let mut s = scroll_state.write();
        s.scroll_to_bottom();
    }

    element! {
        ScrollView(
            state: scroll_state,
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(all_lines))
        }
    }
    .into_any()
}

// ── ViewModel → Line 转换（纯函数，无组件开销） ──────────────────────────────

fn render_vm_to_lines(vm: &TuiRenderUnit, out: &mut Vec<Line<'static>>) {
    match vm {
        TuiRenderUnit::TuiUserBubble(data) => {
            if let Some(ref reminder) = data.reminder {
                out.push(Line::from(Span::styled(
                    reminder.reminder_type.label(),
                    Style::default().fg(ratatui::style::Color::Rgb(153, 153, 153)),
                )));
                out.push(Line::from(Span::styled(
                    reminder.summary.clone(),
                    Style::default().fg(ratatui::style::Color::Rgb(80, 80, 80)),
                )));
            } else {
                let semantic = theme::semantic();
                let user_bg = theme::component().message.user_bg;
                for (i, line) in data.text.lines().enumerate() {
                    let prefix_text = if i == 0 { "❯ " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(
                            prefix_text,
                            Style::default()
                                .fg(semantic.accent)
                                .add_modifier(Modifier::BOLD)
                                .bg(user_bg),
                        ),
                        Span::styled(line.to_string(), Style::default().bg(user_bg)),
                    ]));
                }
            }
        }
        TuiRenderUnit::TuiAssistantBubble(data) => {
            let semantic = theme::semantic();
            // reasoning 块
            if let Some(ref r) = data.reasoning {
                out.push(Line::from(""));
                let char_count = r.text.chars().count();
                out.push(Line::from(vec![Span::styled(
                    i18n::tr_args(
                        "render-thought-for",
                        &[(
                            "count".to_string(),
                            fluent_bundle::FluentValue::from(char_count as u64),
                        )],
                    ),
                    Style::default()
                        .fg(semantic.text.dim)
                        .add_modifier(Modifier::ITALIC),
                )]));
                if !r.collapsed {
                    let tail_lines: Vec<&str> = r.text.lines().rev().take(3).collect();
                    for tail in tail_lines.into_iter().rev() {
                        if !tail.is_empty() {
                            out.push(Line::from(vec![
                                Span::styled(" ⎿ ", Style::default().fg(semantic.text.dim)),
                                Span::styled(
                                    tail.to_string(),
                                    Style::default().fg(semantic.text.dim),
                                ),
                            ]));
                        }
                    }
                }
                out.push(Line::from(""));
            }
            // markdown 正文：按行渲染（纯文本，后续可加 syntax highlight）
            for line in data.text.lines() {
                out.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(semantic.text.primary),
                )));
            }
        }
        TuiRenderUnit::TuiToolCard(data) => {
            let semantic = theme::semantic();
            let collapsed = should_collapse(&data.tool_name, data.is_running, data.is_error);

            let indicator = if data.is_running {
                Span::styled("⏳", Style::default().fg(semantic.status.warning))
            } else if data.is_error {
                Span::styled("✗", Style::default().fg(semantic.status.error))
            } else {
                Span::styled("✓", Style::default().fg(semantic.status.success))
            };

            let display_name = format_tool_name(&data.tool_name).to_string();
            let name = Span::styled(
                display_name,
                Style::default()
                    .fg(semantic.text.primary)
                    .add_modifier(Modifier::BOLD),
            );

            let mut header_spans = vec![indicator, Span::raw(" "), name];

            let summary = truncate(&data.input_summary, 400);
            if !summary.is_empty() {
                header_spans.push(Span::styled(
                    format!(" ({})", summary),
                    Style::default().fg(semantic.text.dim),
                ));
            }
            out.push(Line::from(header_spans));

            // 运行中状态行
            if data.is_running && !data.is_error {
                let duration = data.running_duration_ms.unwrap_or(0);
                if data.tool_name == "Bash" {
                    out.push(Line::from(vec![
                        Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                        Span::styled(
                            format!("Running ({})", format_duration(duration)),
                            Style::default().fg(semantic.text.muted),
                        ),
                    ]));
                } else if data.tool_name == "Agent" && data.tool_calls_count > 0 {
                    out.push(Line::from(vec![
                        Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                        Span::styled(
                            format!(
                                "{} tool calls, running {}",
                                data.tool_calls_count,
                                format_duration(duration)
                            ),
                            Style::default().fg(semantic.text.muted),
                        ),
                    ]));
                }
            }

            if !data.output_summary.is_empty() {
                let color = if data.is_error {
                    semantic.status.error
                } else {
                    semantic.text.muted
                };
                let max_lines = if collapsed {
                    1
                } else if data.tool_name == "TodoWrite" {
                    usize::MAX
                } else {
                    4
                };
                for out_line in compact_lines(&data.output_summary, max_lines, 400) {
                    out.push(Line::from(vec![
                        Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                        Span::styled(out_line, Style::default().fg(color)),
                    ]));
                }
            }

            out.push(Line::from(""));
        }
        TuiRenderUnit::TuiSystemNote(data) => {
            let semantic = theme::semantic();
            for line_text in data.text.lines() {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let color = if line_text.contains('\u{274C}')
                    || line_text.contains("失败")
                    || line_text.contains("error")
                {
                    semantic.status.error
                } else if line_text.contains("warning") || line_text.contains("warn") {
                    semantic.status.warning
                } else {
                    semantic.text.muted
                };

                let content = if line_text.starts_with('\u{273B}') {
                    spans.push(Span::styled(
                        "✻ ".to_string(),
                        Style::default().fg(semantic.text.dim),
                    ));
                    line_text
                        .strip_prefix('\u{273B}')
                        .unwrap_or(line_text)
                        .trim_start()
                } else if line_text.starts_with("  ⎿") {
                    spans.push(Span::styled(
                        "  ⎿ ".to_string(),
                        Style::default().fg(semantic.text.dim),
                    ));
                    line_text
                        .strip_prefix("  ⎿")
                        .unwrap_or(line_text)
                        .trim_start()
                } else if line_text.starts_with('⎿') {
                    spans.push(Span::styled(
                        "⎿ ".to_string(),
                        Style::default().fg(semantic.text.dim),
                    ));
                    line_text
                        .strip_prefix('⎿')
                        .unwrap_or(line_text)
                        .trim_start()
                } else {
                    line_text
                };

                if !content.is_empty() {
                    spans.push(Span::styled(
                        content.to_string(),
                        Style::default().fg(color),
                    ));
                }
                out.push(Line::from(spans));
            }
        }
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            render_subagent_lines(data, out);
        }
        TuiRenderUnit::TuiCollapsedGroup(data) => {
            let semantic = theme::semantic();
            out.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(semantic.status.success)),
                Span::styled(
                    format!("{}（{} 项）", data.title, data.count),
                    Style::default().fg(semantic.text.muted),
                ),
            ]));
        }
        TuiRenderUnit::TuiDivider(data) => {
            if let Some(ref label) = data.label {
                out.push(Line::from(vec![
                    Span::styled(
                        "── ",
                        Style::default().fg(ratatui::style::Color::Rgb(80, 80, 80)),
                    ),
                    Span::styled(
                        label.clone(),
                        Style::default().fg(ratatui::style::Color::Rgb(153, 153, 153)),
                    ),
                    Span::styled(
                        " ──",
                        Style::default().fg(ratatui::style::Color::Rgb(80, 80, 80)),
                    ),
                ]));
            }
            // 空 divider：不输出任何行
        }
        TuiRenderUnit::TuiAskUserBlock(data) => {
            let color = if data.is_error {
                ratatui::style::Color::Rgb(255, 107, 128)
            } else {
                ratatui::style::Color::Rgb(153, 153, 153)
            };
            for item in &data.items {
                out.push(Line::from(Span::styled(
                    format!("Q: {}", item.header),
                    Style::default().fg(color),
                )));
                out.push(Line::from(Span::styled(
                    format!("A: {}", item.answer),
                    Style::default().fg(color),
                )));
            }
            out.push(Line::from(""));
        }
    }
}

fn render_subagent_lines(data: &TuiSubAgentGroup, out: &mut Vec<Line<'static>>) {
    let semantic = theme::semantic();

    // Header
    let arrow = if data.collapsed { "▶" } else { "▼" };
    let mut header_spans = vec![
        Span::styled(
            format!("{} ◆ ", arrow),
            Style::default()
                .fg(semantic.text.dim)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            data.agent_name.clone(),
            Style::default().fg(semantic.text.primary),
        ),
    ];
    if data.is_running {
        header_spans.push(Span::styled(
            " …",
            Style::default()
                .fg(semantic.status.warning)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    out.push(Line::from(header_spans));

    // 递归渲染子 ViewModel
    if !data.collapsed {
        for child in &data.view_models {
            // 缩进占位——子内容暂以 "…" 占位
            out.push(Line::from(Span::styled(
                "  …",
                Style::default().fg(semantic.text.dim),
            )));
            let _ = child;
        }
    }

    out.push(Line::from(""));
}

fn build_footer_lines(is_loading: bool, todos: &[TodoItem], out: &mut Vec<Line<'static>>) {
    let semantic = theme::semantic();

    if is_loading {
        out.push(Line::from(vec![Span::styled(
            i18n::tr("loading"),
            Style::default().fg(semantic.loading),
        )]));
    }

    for todo in todos {
        let icon = match todo.status {
            TodoStatus::Completed => "✔",
            TodoStatus::InProgress => "◼",
            TodoStatus::Pending => "◻",
        };
        out.push(Line::from(vec![
            Span::styled(
                format!("{} ", icon),
                Style::default().fg(semantic.status.success),
            ),
            Span::styled(
                todo.content.clone(),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }
}

// ── ToolCard helpers ─────────────────────────────────────────────────────────

const COLLAPSED_BY_DEFAULT: &[&str] = &["Bash", "Read", "Glob", "Grep", "AskUserQuestion"];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool", "SearchExtraTools"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];

fn should_collapse(tool_name: &str, is_running: bool, is_error: bool) -> bool {
    if is_error {
        return false;
    }
    if AUTO_EXPAND.contains(&tool_name) {
        return false;
    }
    if FORCE_EXPAND_ON_COMPLETE.contains(&tool_name) {
        return is_running;
    }
    COLLAPSED_BY_DEFAULT.contains(&tool_name)
}

fn format_tool_name(raw: &str) -> &str {
    match raw {
        "Bash" => "Shell",
        "folder_operations" => "Folder",
        other => other,
    }
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{}m{}s", mins, secs)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

fn compact_lines(output: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let start = if lines.len() > max_lines {
        lines.len() - max_lines
    } else {
        0
    };
    lines[start..]
        .iter()
        .map(|line| {
            if line.chars().count() > max_chars {
                format!("{}...", line.chars().take(max_chars).collect::<String>())
            } else {
                line.to_string()
            }
        })
        .collect()
}

// ── Todo 类型 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}
