use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::{
    message_view::{AgentSummary, ContentBlockView, MessageViewModel, ToolCategory},
    theme,
};

/// Generate always-visible error summary lines (up to 400 Unicode chars).
/// 2-space indent, no vertical bar, no prefix. Preserves newlines (multi-line render).
fn error_summary_lines(content: &str) -> Vec<Line<'static>> {
    let truncated: String = content.chars().take(400).collect();
    truncated
        .lines()
        .map(|line| {
            Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(theme::DIM)),
                Span::styled(line.to_string(), Style::default().fg(theme::ERROR)),
            ])
        })
        .collect()
}

/// 批次汇总树形渲染：折叠态显示 header + 每行摘要，展开态显示各 agent 详情。
fn render_batch_summary(agents: &[AgentSummary], collapsed: &bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let total = agents.len();
    let failed_count = agents.iter().filter(|a| a.is_error).count();

    // Header 行
    let header_text = if failed_count == total {
        // 全部失败
        format!("{} agents failed", total)
    } else if failed_count > 0 {
        // 部分失败
        format!("{} agents finished, {} failed", total, failed_count)
    } else {
        format!("{} agents finished", total)
    };
    lines.push(Line::from(vec![
        Span::styled("● ", Style::default().fg(theme::SAGE)),
        Span::styled(header_text, Style::default().fg(theme::TEXT)),
    ]));

    if *collapsed {
        // 折叠态：每行 agent 摘要
        for (idx, agent) in agents.iter().enumerate() {
            let is_last = idx == total - 1;
            let connector = if is_last { "└─" } else { "├─" };
            let status = if agent.is_error {
                ("Failed", theme::ERROR)
            } else {
                ("Done", theme::SAGE)
            };

            let mut spans = vec![
                Span::styled("   ", Style::default().fg(theme::DIM)),
                Span::styled(connector.to_string(), Style::default().fg(theme::DIM)),
                Span::styled(" ".to_string(), Style::default()),
                Span::styled(agent.task_preview.clone(), Style::default().fg(theme::TEXT)),
            ];

            if agent.tool_count > 0 {
                spans.push(Span::styled(
                    format!(" · {} tool uses", agent.tool_count),
                    Style::default().fg(theme::DIM),
                ));
            }

            spans.push(Span::styled(" · ", Style::default().fg(theme::DIM)));
            spans.push(Span::styled(
                status.0.to_string(),
                Style::default().fg(status.1),
            ));

            lines.push(Line::from(spans));
        }
    } else {
        // 展开态：每个 agent 显示 task_preview + final_result
        for (idx, agent) in agents.iter().enumerate() {
            let is_last = idx == total - 1;
            let connector = if is_last { "└─" } else { "├─" };

            // task_preview 行
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(connector.to_string(), Style::default().fg(theme::DIM)),
                Span::raw(" "),
                Span::styled(agent.task_preview.clone(), Style::default().fg(theme::TEXT)),
            ]));

            // final_result 行（如果有）
            if let Some(ref result) = agent.final_result {
                if !result.is_empty() {
                    lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled("⎿ ", Style::default().fg(theme::DIM)),
                        Span::styled(result.clone(), Style::default().fg(theme::MUTED)),
                    ]));
                }
            }
        }
    }

    lines
}

/// AskUserQuestion 专用渲染：`● User answered Peri's questions:` + `⎿ · H → V`
fn render_ask_user_block(content: &str, is_error: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let color = if is_error { theme::ERROR } else { theme::SAGE };
    lines.push(Line::from(vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::styled(
            "User answered Peri's questions:".to_string(),
            Style::default().fg(theme::TEXT),
        ),
    ]));

    if content.is_empty() {
        return lines;
    }

    // 解析多问题格式: [问: H]\n回答: V\n\n[问: H2]\n回答: V2
    for block in content.split("\n\n") {
        let mut header = String::new();
        let mut answer = String::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("[问: ") {
                header = rest.trim_end_matches(']').to_string();
            } else if let Some(a) = line.strip_prefix("回答: ") {
                answer = a.to_string();
            }
        }
        header = header.replace(['\n', '\r'], " ");
        answer = answer.replace(['\n', '\r'], " ");
        let text = if !header.is_empty() {
            format!("{} → {}", header, answer)
        } else if !answer.is_empty() {
            answer
        } else {
            block.lines().collect::<Vec<_>>().join(" ")
        };
        if text.is_empty() {
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled("  ⎿ ", Style::default().fg(theme::DIM)),
            Span::styled(
                text,
                Style::default().fg(if is_error { theme::ERROR } else { theme::MUTED }),
            ),
        ]));
    }

    lines
}

/// 将单个 ViewModel 渲染为 Vec<Line>
pub fn render_view_model(
    vm: &MessageViewModel,
    _index: Option<usize>,
    _width: usize,
    diff_visible: bool,
) -> Vec<Line<'static>> {
    match vm {
        MessageViewModel::UserBubble {
            rendered,
            system_reminder,
            ..
        } => {
            if *system_reminder {
                // 系统提醒：渲染一行简略提示（文案与 en locale 保持一致；
                // zh-CN locale 因 TUI 未接入 i18n handler 而硬编码，详见
                // spec/superpowers/specs/2026-06-02-system-reminder-compact-summary-design.md）
                let hint = Span::styled(
                    "\u{1f4cb} Context compacted",
                    Style::default()
                        .fg(theme::DIM)
                        .add_modifier(Modifier::ITALIC),
                );
                return vec![Line::from(hint)];
            }
            // 普通 UserBubble — 原有渲染逻辑不变
            let user_bg: Color = theme::USER_BG;
            let mut lines = Vec::with_capacity(rendered.lines.len() + 1);
            for (i, line) in rendered.lines.iter().enumerate() {
                if i == 0 {
                    // 第一行：用户消息用 ❯ 前缀，带底色
                    let mut spans = vec![Span::styled(
                        "❯ ",
                        Style::default()
                            .fg(theme::ACCENT)
                            .add_modifier(Modifier::BOLD)
                            .bg(user_bg),
                    )];
                    for span in &line.spans {
                        spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
                    }
                    lines.push(Line::from(spans));
                } else {
                    // 后续行：填充 + 原始 spans，带底色
                    let mut spans = vec![Span::styled("  ", Style::default().bg(user_bg))];
                    for span in &line.spans {
                        spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
                    }
                    lines.push(Line::from(spans));
                }
            }
            lines
        }
        MessageViewModel::AssistantBubble { blocks, .. } => {
            let mut lines = Vec::new();

            for block in blocks {
                match block {
                    ContentBlockView::Text { rendered, raw, .. } => {
                        let is_diff = peri_widgets::message_block::highlight::is_diff_content(raw);
                        if is_diff {
                            for l in raw.lines() {
                                let diff_spans =
                                    peri_widgets::message_block::highlight::highlight_diff_line(
                                        l,
                                        &peri_widgets::DarkTheme,
                                    );
                                lines.push(Line::from(diff_spans));
                            }
                        } else {
                            for line in rendered.lines.iter() {
                                lines.push(Line::from(line.spans.clone()));
                            }
                        }
                    }
                    ContentBlockView::Reasoning {
                        char_count,
                        tail_lines,
                        ..
                    } => {
                        lines.push(Line::from(vec![Span::styled(
                            format!("Thought for {} chars", char_count),
                            Style::default().fg(theme::DIM),
                        )]));
                        if let Some(tail) = tail_lines {
                            for tail_line in tail.lines() {
                                lines.push(Line::from(vec![
                                    Span::styled(" ⎿ ", Style::default().fg(theme::DIM)),
                                    Span::styled(
                                        tail_line.to_string(),
                                        Style::default().fg(theme::DIM),
                                    ),
                                ]));
                            }
                        }
                    }
                    ContentBlockView::ToolUse { .. } => {
                        // AI 消息不再显示工具调用行
                    }
                }
            }

            lines
        }
        MessageViewModel::ToolBlock {
            collapsed,
            display_name,
            args_display,
            content,
            color: _color,
            is_error,
            tool_name,
            diff_lines,
            ..
        } => {
            // AskUserQuestion 专用渲染路径
            if tool_name == "AskUserQuestion" {
                return render_ask_user_block(content, *is_error);
            }

            let is_running = content.is_empty() && !*is_error;

            // 构建状态（仅用于 result_lines 管理）
            let status = if *is_error {
                peri_widgets::ToolCallStatus::Failed
            } else if is_running {
                peri_widgets::ToolCallStatus::Running
            } else {
                peri_widgets::ToolCallStatus::Completed
            };

            // Write/Edit 工具完成后默认展开（显示写入/编辑结果摘要）
            let effective_collapsed =
                if !is_running && (tool_name == "Write" || tool_name == "Edit") {
                    false
                } else {
                    *collapsed
                };
            let mut state = peri_widgets::ToolCallState::new(display_name.clone(), theme::TEXT);
            state.status = status;
            state.collapsed = effective_collapsed;
            state.is_error = *is_error;
            if let Some(args) = args_display {
                state.args_summary = args.clone();
            }
            if !content.is_empty() {
                state.set_result(content.clone());
            }

            let tool_color = if *is_error { theme::ERROR } else { theme::SAGE };

            // ● 指示器：运行中闪烁，完成固定，失败 ✗
            let indicator = if is_running {
                let tick = std::time::Instant::now().elapsed().as_millis() as u64 / 200;
                if (tick / 4).is_multiple_of(2) {
                    "●"
                } else {
                    " "
                }
            } else if *is_error {
                "✗"
            } else {
                "●"
            };

            let mut header_spans = vec![
                Span::styled(indicator.to_string(), Style::default().fg(tool_color)),
                Span::raw(" "),
                Span::styled(
                    state.tool_name.clone(),
                    Style::default()
                        .fg(theme::TEXT)
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            if !state.args_summary.is_empty() {
                let summary =
                    peri_widgets::tool_call::display::format_args_summary(&state.args_summary, 400);
                header_spans.push(Span::styled(
                    format!("({})", summary),
                    Style::default().fg(theme::DIM),
                ));
            }
            let mut lines = vec![Line::from(header_spans)];
            if !state.collapsed && !state.result_lines.is_empty() {
                let result_color = if *is_error {
                    theme::ERROR
                } else {
                    theme::MUTED
                };
                let border_color = if *is_error { theme::ERROR } else { theme::DIM };
                for line in &state.result_lines {
                    lines.push(Line::from(vec![
                        Span::styled("  ⎿ ".to_string(), Style::default().fg(border_color)),
                        Span::styled(line.clone(), Style::default().fg(result_color)),
                    ]));
                }
            } else if *is_error && !content.is_empty() {
                lines.extend(error_summary_lines(content));
            }
            // 内嵌 diff 视图（预渲染缓存，默认关闭，Ctrl+O 切换）
            if diff_visible {
                if let Some(ref cached_lines) = diff_lines {
                    lines.extend(cached_lines.iter().cloned());
                }
            }
            lines
        }
        MessageViewModel::SubAgentGroup {
            batch_agents,
            collapsed,
            ..
        } if !batch_agents.is_empty() => render_batch_summary(batch_agents, collapsed),
        MessageViewModel::SubAgentGroup {
            agent_id,
            task_preview,
            recent_messages,
            collapsed,
            is_error,
            is_running,
            is_background,
            bg_hash,
            final_result,
            ..
        } => {
            let agent_color = if *is_error {
                theme::ERROR
            } else if *is_running && *is_background {
                theme::WARNING
            } else {
                theme::SAGE
            };
            let mut lines: Vec<Line<'static>> = Vec::new();

            if *collapsed {
                // 折叠状态：两行显示
                // Header: ❯ Agent(type) #hash
                let arrow_color = theme::LOADING; // 淡蓝紫色 #93A5FF
                let mut header_spans = vec![
                    Span::styled("❯ ".to_string(), Style::default().fg(arrow_color)),
                    Span::styled(
                        "Agent".to_string(),
                        Style::default()
                            .fg(agent_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({})", agent_id), Style::default().fg(theme::MUTED)),
                ];
                // 折叠状态显示短 hash
                if let Some(ref hash) = bg_hash {
                    header_spans.push(Span::styled(
                        format!(" #{}", hash),
                        Style::default().fg(theme::MUTED),
                    ));
                }
                lines.push(Line::from(header_spans));

                let task_label: String = task_preview.chars().take(50).collect();
                let suffix = if task_preview.chars().count() > 50 {
                    "…"
                } else {
                    ""
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}{}", task_label, suffix),
                    Style::default().fg(theme::MUTED),
                )]));
                if *is_error {
                    if let Some(ref result) = final_result {
                        if !result.is_empty() {
                            lines.extend(error_summary_lines(result));
                        }
                    }
                }
            } else {
                // 展开状态：名称 + 任务描述
                // Header: ❯ Agent(type) #hash
                let arrow_color = theme::LOADING; // 淡蓝紫色 #93A5FF
                let mut header_spans = vec![
                    Span::styled("❯ ".to_string(), Style::default().fg(arrow_color)),
                    Span::styled(
                        "Agent".to_string(),
                        Style::default()
                            .fg(agent_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({})", agent_id), Style::default().fg(theme::MUTED)),
                ];
                // 展开状态显示短 hash
                if let Some(ref hash) = bg_hash {
                    header_spans.push(Span::styled(
                        format!(" #{}", hash),
                        Style::default().fg(theme::MUTED),
                    ));
                }
                lines.push(Line::from(header_spans));

                let task_label: String = task_preview.chars().take(50).collect();
                let suffix = if task_preview.chars().count() > 50 {
                    "…"
                } else {
                    ""
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}{}", task_label, suffix),
                    Style::default().fg(theme::MUTED),
                )]));

                // 嵌套消息（不渲染序号），跳过无可见内容的条目
                // 当有 final_result 时，跳过最后一条消息（其内容已包含在 final_result 中）
                let has_final = final_result.as_ref().is_some_and(|r| !r.is_empty());
                let skip_last = has_final && recent_messages.len() > 1;
                let iter_messages: &[MessageViewModel] = if skip_last {
                    &recent_messages[..recent_messages.len() - 1]
                } else {
                    recent_messages
                };
                for inner_vm in iter_messages.iter() {
                    // SubAgent 内部跳过 AssistantBubble，只显示工具调用
                    if matches!(inner_vm, MessageViewModel::AssistantBubble { .. }) {
                        continue;
                    }
                    let inner_lines = render_view_model(inner_vm, None, _width, diff_visible);
                    if inner_lines.is_empty() {
                        continue;
                    }
                    for line in inner_lines {
                        // 每行前缀 2 空格缩进
                        let mut new_spans = vec![Span::raw("  ")];
                        new_spans.extend(line.spans);
                        lines.push(Line::from(new_spans));
                    }
                }
                // 移除尾部空行
                while lines.last().is_some_and(|l| l.spans.is_empty()) {
                    lines.pop();
                }

                // 子 agent 完成后，渲染 final_result 摘要（仅第一行）
                if let Some(ref result) = final_result {
                    if !result.is_empty() {
                        if let Some(first_line) = result.lines().next() {
                            if !first_line.is_empty() {
                                let text: String = first_line.chars().take(80).collect();
                                lines.push(Line::from(vec![
                                    Span::styled("  ⎿ ", Style::default().fg(theme::DIM)),
                                    Span::styled(text, Style::default().fg(theme::MUTED)),
                                ]));
                            }
                        }
                    }
                }
            }

            lines
        }
        MessageViewModel::SystemNote { content, .. } => {
            let mut lines = Vec::new();
            for line in content.lines() {
                if line.starts_with('✻') {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(theme::DIM),
                    )));
                } else if line.starts_with('⎿') {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(theme::MUTED),
                    )));
                } else {
                    let is_error =
                        line.contains("❌") || line.contains("失败") || line.contains("错误");
                    let is_warn = line.contains("⚠") || line.contains("已中断");
                    let text_color = if is_error {
                        theme::ERROR
                    } else if is_warn {
                        theme::WARNING
                    } else {
                        theme::MUTED
                    };
                    lines.push(Line::from(vec![
                        Span::styled("· ", Style::default().fg(theme::DIM)),
                        Span::styled(line.to_string(), Style::default().fg(text_color)),
                    ]));
                }
            }
            lines
        }
        MessageViewModel::CacheWarning { content, .. } => {
            vec![Line::from(Span::styled(
                content.clone(),
                Style::default().fg(theme::WARNING),
            ))]
        }
        MessageViewModel::ToolCallGroup {
            category,
            tools,
            collapsed: _collapsed,
            ..
        } => {
            let mut lines = Vec::new();

            if *category == ToolCategory::AskUser {
                // AskUserQuestion 聚合：统一标题 + 所有问答对
                let has_error = tools.iter().any(|t| t.is_error);
                let color = if has_error { theme::ERROR } else { theme::SAGE };
                lines.push(Line::from(vec![
                    Span::styled("● ", Style::default().fg(color)),
                    Span::styled(
                        "User answered Peri's questions:".to_string(),
                        Style::default().fg(theme::TEXT),
                    ),
                ]));

                for entry in tools {
                    let entry_color = if entry.is_error {
                        theme::ERROR
                    } else {
                        theme::MUTED
                    };
                    if entry.content.is_empty() {
                        continue;
                    }
                    // 解析每个工具结果中的问答对
                    for block in entry.content.split("\n\n") {
                        let mut header = String::new();
                        let mut answer = String::new();
                        for line in block.lines() {
                            if let Some(rest) = line.strip_prefix("[问: ") {
                                header = rest.trim_end_matches(']').to_string();
                            } else if let Some(a) = line.strip_prefix("回答: ") {
                                answer = a.to_string();
                            }
                        }
                        header = header.replace(['\n', '\r'], " ");
                        answer = answer.replace(['\n', '\r'], " ");
                        let text = if !header.is_empty() {
                            format!("{} → {}", header, answer)
                        } else if !answer.is_empty() {
                            answer
                        } else {
                            block.lines().collect::<Vec<_>>().join(" ")
                        };
                        if text.is_empty() {
                            continue;
                        }
                        lines.push(Line::from(vec![
                            Span::styled("  ⎿ ", Style::default().fg(theme::DIM)),
                            Span::styled(text, Style::default().fg(entry_color)),
                        ]));
                    }
                }
            } else {
                let summary = ToolCategory::summary_for_tools(tools);

                // 统一 ● 前缀，仅显示汇总行
                lines.push(Line::from(vec![
                    Span::styled("● ", Style::default().fg(theme::SAGE)),
                    Span::styled(summary, Style::default().fg(theme::MUTED)),
                ]));
                // 显示出错工具的错误摘要
                for entry in tools {
                    if entry.is_error && !entry.content.is_empty() {
                        lines.extend(error_summary_lines(&entry.content));
                    }
                }
            }

            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::message_view::AgentSummary;
    include!("message_render_test.rs");
}
