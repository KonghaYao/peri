//! 后台 Agent 管理栏——显示运行中的后台 SubAgent 列表

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem},
};

use crate::app::App;

/// 固定调色板（最多 8 种颜色循环）
const AGENT_COLORS: &[Color] = &[
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::Blue,
    Color::Red,
    Color::Rgb(255, 165, 0),   // orange
    Color::Rgb(148, 103, 189), // purple
];

/// 获取 agent 在列表中对应的颜色
pub(crate) fn agent_color(index: usize) -> Color {
    AGENT_COLORS[index % AGENT_COLORS.len()]
}

/// 格式化耗时（秒级）
fn format_elapsed(start: std::time::Instant) -> String {
    let secs = start.elapsed().as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// 计算 bar 需要的高度（0 = 隐藏）
pub(crate) fn bg_bar_height(app: &App) -> u16 {
    let count = app.session_mgr.current().background_agents.len();
    if count == 0 {
        0
    } else {
        // 1 行 main + N 行 agent + 1 行底部空行，最多 7 行
        let content = (1 + count.min(4)).min(5);
        (content + 1) as u16
    }
}

pub(crate) fn render_bg_agent_bar(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let session = &app.session_mgr.current();
    let agents = &session.background_agents;
    let focused_id = &session.focused_instance_id;
    let visible_count = agents.len().min(4);
    let total_items = 1 + visible_count;
    let cursor = session.ui.bg_bar_cursor.map(|c| c.min(total_items - 1));

    let mut items: Vec<ListItem> = Vec::new();

    // 第 1 行：main
    let main_selected = cursor == Some(0);
    let main_focused = focused_id.is_none();
    let main_display_style = if main_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if main_focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    items.push(ListItem::new(Line::from(vec![
        Span::styled("● ", Style::default().fg(Color::Green)),
        Span::styled("main", main_display_style),
    ])));

    // 后续行：每个后台 agent
    for (i, agent) in agents.iter().take(visible_count).enumerate() {
        let color = agent_color(i);
        let is_focused = focused_id.as_deref() == Some(&agent.instance_id);
        let is_selected = cursor == Some(i + 1);

        let elapsed = format_elapsed(agent.started_at);
        let steps = agent.tool_count;
        let name_preview: String = agent.agent_name.chars().take(20).collect();
        let style = if is_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if is_focused {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };

        items.push(ListItem::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(format!("{:<20}", name_preview), style),
            Span::styled(
                format!(" {}calls ", steps),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(elapsed, Style::default().fg(Color::DarkGray)),
        ])));
    }

    // 溢出提示
    if agents.len() > 4 {
        let overflow = agents.len() - 4;
        items.push(ListItem::new(Line::from(Span::styled(
            format!("  …+{}", overflow),
            Style::default().fg(Color::DarkGray),
        ))));
    }

    // 底部空行，与终端底部保持间距
    items.push(ListItem::new(Line::from("")));

    let list = List::new(items);
    f.render_widget(list, area);
}
