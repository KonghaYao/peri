use peri_widgets::BorderedPanel;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{
        status_panel::{StatusPanel, STATUS_TAB_CONTEXT, STATUS_TAB_COST},
        App,
    },
    ui::theme,
};

pub(crate) fn render_status_panel(f: &mut Frame, panel: &StatusPanel, app: &mut App, area: Rect) {
    let inner = BorderedPanel::new(Span::styled(
        " Status ",
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::BORDER))
    .render(f, area);

    app.session_mgr.current_mut().ui.panel_area = Some(inner);

    // Tab 栏（1 行）
    let tab_height = 1u16;
    let tab_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: tab_height,
    };
    let content_area = Rect {
        x: inner.x,
        y: inner.y + tab_height + 1,
        width: inner.width,
        height: inner.height.saturating_sub(tab_height + 1),
    };

    // 手动渲染 tab 标签（仿照 plugin 面板风格）
    let tab_labels: Vec<Span> = ["Cost", "Context"]
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let is_active = panel.tab.active() == i;
            let style = if is_active {
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::MUTED)
            };
            Span::styled(format!(" {} ", label), style)
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(tab_labels)), tab_area);

    match panel.tab.active() {
        STATUS_TAB_COST => {
            let lines = build_cost_lines(app);
            f.render_widget(Paragraph::new(Text::from(lines)), content_area);
        }
        STATUS_TAB_CONTEXT => {
            render_context_tab(f, app, content_area);
        }
        _ => {}
    }
}

fn build_cost_lines(app: &App) -> Vec<Line<'static>> {
    let tracker = &app.session_mgr.current().agent.session_token_tracker;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 会话时长
    let duration_str = match app.session_mgr.current().agent.session_start_time {
        Some(start) => {
            let s = start.elapsed().as_secs();
            if s >= 3600 {
                format!("{}h{}m{}s", s / 3600, (s % 3600) / 60, s % 60)
            } else if s >= 60 {
                format!("{}m{}s", s / 60, s % 60)
            } else {
                format!("{}s", s)
            }
        }
        None => "N/A".to_string(),
    };
    lines.push(label_value("会话时长", &duration_str));
    lines.push(Line::from(""));

    // Token 消耗
    lines.push(label_value(
        "输入 Tokens",
        &format_number(tracker.total_input_tokens),
    ));
    lines.push(label_value(
        "输出 Tokens",
        &format_number(tracker.total_output_tokens),
    ));
    lines.push(label_value(
        "Cache 创建",
        &format_number(tracker.total_cache_creation_tokens),
    ));
    lines.push(label_value(
        "Cache 读取",
        &format_number(tracker.total_cache_read_tokens),
    ));
    lines.push(Line::from(""));

    // LLM 调用次数
    lines.push(label_value(
        "LLM 调用次数",
        &tracker.llm_call_count.to_string(),
    ));
    lines.push(Line::from(""));

    // 估算费用
    let cost = estimate_cost(app);
    lines.push(label_value("估算费用", &format!("${:.4}", cost)));
    lines.push(Line::from(""));

    // 当前模型
    lines.push(label_value("当前模型", &app.services.model_name));

    lines
}

fn label_value(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<16}", label),
            Style::default().fg(theme::MUTED),
        ),
        Span::styled(
            value.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 基于模型 alias 的简化费用估算
fn estimate_cost(app: &App) -> f64 {
    let tracker = &app.session_mgr.current().agent.session_token_tracker;
    let alias_owned = app.services.peri_config.read().config.active_alias.clone();
    let alias = alias_owned.as_str();

    let (input_price, output_price) = match alias {
        "opus" => (15.0, 75.0),
        "haiku" => (0.80, 4.0),
        _ => (3.0, 15.0), // sonnet default
    };

    let input_cost = (tracker.total_input_tokens as f64 / 1_000_000.0) * input_price;
    let output_cost = (tracker.total_output_tokens as f64 / 1_000_000.0) * output_price;
    input_cost + output_cost
}

/// 计算一个"漂亮"的上界值（向上取整到 1/2/5 × 10^n）
fn nice_ceil(value: u64) -> u64 {
    if value == 0 {
        return 1;
    }
    let magnitude = 10u64.pow(value.ilog10());
    let normalized = value as f64 / magnitude as f64;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    (nice * magnitude as f64) as u64
}

fn build_bar_chart_lines(
    history: &[peri_agent::agent::token::RequestRecord],
    chart_width: usize,
    chart_height: usize,
) -> Vec<Line<'static>> {
    use ratatui::{style::Style, text::Span};

    if history.is_empty() || chart_height == 0 || chart_width == 0 {
        return vec![];
    }

    let start = history.len().saturating_sub(chart_width);
    let visible = &history[start..];

    let max_input = visible
        .iter()
        .map(|r| r.input_tokens as u64)
        .max()
        .unwrap_or(1);
    let y_max = nice_ceil(max_input);

    let mut lines = Vec::with_capacity(chart_height + 1);

    for row in (0..chart_height).rev() {
        let row_bottom = y_max * row as u64 / chart_height as u64;
        let label = format_number(y_max * (row + 1) as u64 / chart_height as u64);

        let mut spans: Vec<Span> = vec![Span::styled(
            format!("{:>6}┤", label),
            Style::default().fg(theme::MUTED),
        )];

        for record in visible {
            let input = record.input_tokens as u64;
            if input < row_bottom {
                spans.push(Span::raw(" "));
            } else {
                let cache_read = record.cache_read_input_tokens as u64;
                let cache_create = record.cache_creation_input_tokens as u64;
                let cache_top = cache_read + cache_create;

                let color = if row_bottom < cache_read {
                    theme::SAGE
                } else if row_bottom < cache_top {
                    theme::WARNING
                } else {
                    theme::ACCENT
                };
                spans.push(Span::styled("█", Style::default().fg(color)));
            }
        }

        lines.push(Line::from(spans));
    }

    // 底部 x 轴线
    let mut axis_spans: Vec<Span> = vec![Span::styled(
        "     0┼".to_string(),
        Style::default().fg(theme::MUTED),
    )];
    for _ in visible {
        axis_spans.push(Span::styled("─", Style::default().fg(theme::DIM)));
    }
    lines.push(Line::from(axis_spans));

    lines
}

/// 构建缓存命中率柱状图（y 轴百分比刻度，█ 填充，与 token 柱状图风格统一）
fn build_cache_rate_lines(
    history: &[peri_agent::agent::token::RequestRecord],
    chart_width: usize,
    chart_height: usize,
) -> Vec<Line<'static>> {
    use ratatui::{style::Style, text::Span};

    if history.is_empty() || chart_height == 0 || chart_width == 0 {
        return vec![];
    }

    let start = history.len().saturating_sub(chart_width);
    let visible = &history[start..];

    // y 轴自适应：根据实际数据范围计算刻度
    let rates: Vec<u64> = visible
        .iter()
        .map(|r| (r.cache_hit_rate() * 100.0) as u64)
        .collect();
    let rate_min = *rates.iter().min().unwrap_or(&0);
    let rate_max = *rates.iter().max().unwrap_or(&100);

    let (y_min, y_max) = if rate_min == rate_max {
        // 所有值相同，加 ±5 padding
        (rate_min.saturating_sub(5), (rate_min + 5).min(100))
    } else {
        let range = rate_max - rate_min;
        let pad = (range as f64 * 0.05).ceil().max(1.0) as u64;
        let y_min = rate_min.saturating_sub(pad);
        let y_max = nice_ceil(rate_max + pad).min(100);
        (y_min, y_max)
    };
    let y_range = y_max - y_min;

    let mut lines = Vec::with_capacity(chart_height + 1);

    for row in (0..chart_height).rev() {
        let row_bottom = y_min + y_range * row as u64 / chart_height as u64;
        let row_top = y_min + y_range * (row + 1) as u64 / chart_height as u64;
        let label = format!("{}%", row_top);

        let mut spans: Vec<Span> = vec![Span::styled(
            format!("{:>6}┤", label),
            Style::default().fg(theme::MUTED),
        )];

        for record in visible {
            let rate = (record.cache_hit_rate() * 100.0) as u64;
            if rate < row_bottom {
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::styled("█", Style::default().fg(theme::SAGE)));
            }
        }

        lines.push(Line::from(spans));
    }

    // 底部 x 轴线
    let mut axis_spans: Vec<Span> = vec![Span::styled(
        format!("{:>6}%┼", y_min),
        Style::default().fg(theme::MUTED),
    )];
    for _ in visible {
        axis_spans.push(Span::styled("─", Style::default().fg(theme::DIM)));
    }
    lines.push(Line::from(axis_spans));

    lines
}

fn build_x_axis_labels(visible_start: usize, visible_len: usize) -> Line<'static> {
    use ratatui::{style::Style, text::Span};

    let label_every = if visible_len <= 10 {
        1
    } else if visible_len <= 20 {
        5
    } else if visible_len <= 50 {
        10
    } else {
        20
    };

    let mut spans: Vec<Span> = vec![Span::raw("       ")];

    for i in 0..visible_len {
        let req_num = visible_start + i + 1;
        if req_num.is_multiple_of(label_every) || i == 0 || i == visible_len - 1 {
            spans.push(Span::styled(
                req_num.to_string(),
                Style::default().fg(theme::MUTED),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
    }

    Line::from(spans)
}

fn build_context_summary(app: &App) -> Line<'static> {
    use ratatui::{
        style::{Modifier, Style},
        text::Span,
    };

    let tracker = &app.session_mgr.current().agent.session_token_tracker;
    let context_window = app.session_mgr.current().agent.context_window;
    let msg_count = app.session_mgr.current().agent.origin_messages.len();
    let tool_count = app.session_mgr.current().agent.tool_call_count;

    let used = tracker.estimated_context_tokens().unwrap_or(0);
    let pct = tracker
        .context_usage_percent(context_window)
        .map(|p| format!("{:.1}%", p))
        .unwrap_or_else(|| "N/A".to_string());

    Line::from(vec![
        Span::styled("  上下文: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            format_number(context_window as u64),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | 已用: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            format!("{} ({})", format_number(used), pct),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | 消息: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            msg_count.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | 工具: ", Style::default().fg(theme::MUTED)),
        Span::styled(
            tool_count.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn render_context_tab(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::{
        style::Style,
        text::{Line, Span, Text},
        widgets::Paragraph,
    };

    let history = &app
        .session_mgr
        .current()
        .agent
        .session_token_tracker
        .request_history;

    if history.is_empty() {
        let mut lines = vec![build_context_summary(app)];
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  暂无请求数据",
            Style::default().fg(theme::MUTED),
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), area);
        return;
    }

    let summary_h = 1u16;
    let legend_h = 1u16;
    let x_axis_h = 1u16;
    let rate_title_h = 1u16;
    let rate_h = 4u16;
    let rate_x_axis_h = 1u16;
    let blanks = 2u16;

    let chart_h = area.height.saturating_sub(
        summary_h + legend_h + x_axis_h + rate_title_h + rate_h + rate_x_axis_h + blanks,
    );

    let skip_chart = chart_h < 3;
    let actual_blanks = if skip_chart { 1 } else { blanks };

    let mut y = area.y;

    // 摘要行
    f.render_widget(
        Paragraph::new(Text::from(vec![build_context_summary(app)])),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
    y += summary_h;
    y += 1; // 摘要行与图表之间的空行

    if !skip_chart {
        // 图例
        f.render_widget(
            Paragraph::new(Text::from(vec![Line::from(vec![
                Span::styled("  Input Tokens", Style::default().fg(theme::MUTED)),
                Span::styled("  █cache_read", Style::default().fg(theme::SAGE)),
                Span::styled(" █cache_creation", Style::default().fg(theme::WARNING)),
                Span::styled(" █raw", Style::default().fg(theme::ACCENT)),
            ])])),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
        y += legend_h;

        // 柱状图
        let chart_width = (area.width as usize).saturating_sub(7);
        let visible_start = history.len().saturating_sub(chart_width);
        let chart_lines = build_bar_chart_lines(history, chart_width, chart_h as usize);
        f.render_widget(
            Paragraph::new(Text::from(chart_lines)),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: chart_h,
            },
        );
        y += chart_h;

        // x 轴标签
        let visible_len = history.len() - visible_start;
        f.render_widget(
            Paragraph::new(Text::from(vec![build_x_axis_labels(
                visible_start,
                visible_len,
            )])),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
        y += x_axis_h + 1;
    } else {
        y += actual_blanks;
    }

    // 缓存命中率柱状图标题 + 图例
    f.render_widget(
        Paragraph::new(Text::from(vec![Line::from(vec![
            Span::styled("  Cache Hit Rate", Style::default().fg(theme::MUTED)),
            Span::styled("  █hit", Style::default().fg(theme::SAGE)),
        ])])),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
    y += rate_title_h;

    // 缓存命中率折线图（带 y 轴百分比刻度）
    let rate_width = (area.width as usize).saturating_sub(7);
    let rate_start = history.len().saturating_sub(rate_width);
    let rate_lines = build_cache_rate_lines(history, rate_width, rate_h as usize);
    f.render_widget(
        Paragraph::new(Text::from(rate_lines)),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: rate_h,
        },
    );
    y += rate_h;

    // 折线图 x 轴标签
    let rate_visible_len = history.len() - rate_start;
    f.render_widget(
        Paragraph::new(Text::from(vec![build_x_axis_labels(
            rate_start,
            rate_visible_len,
        )])),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use peri_agent::agent::token::RequestRecord;

    use super::*;
    include!("status_test.rs");
}
