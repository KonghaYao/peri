//! ratatui-kit AskUserPopup component.
//!
//! 用户问答弹窗：显示多个问题，支持 Tab 切换焦点、Backspace 编辑输入、
//! Enter 提交、Esc 取消。
//!
//! Phase 7：完整 UI + 键盘导航。Phase 8 接入 on_submit/on_cancel Handler。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::Paragraph,
    },
};

use crate::ui::theme;

/// 单个问题
#[derive(Clone)]
struct QuestionItem {
    question: &'static str,
    hint: &'static str,
}

/// 示例问题数据
fn mock_questions() -> Vec<QuestionItem> {
    vec![
        QuestionItem {
            question: "What is the name of the new module?",
            hint: "e.g. user_service",
        },
        QuestionItem {
            question: "Which directory should it be created in?",
            hint: "e.g. src/services/",
        },
        QuestionItem {
            question: "Do you want to generate tests?",
            hint: "yes / no",
        },
    ]
}

#[component]
pub fn AskUserPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let questions = mock_questions();
    let question_count = questions.len();

    // 当前焦点问题索引
    let focused = hooks.use_state(|| 0usize);
    // 每个问题的输入文本
    let inputs = hooks.use_state(|| vec![String::new(); question_count]);

    hooks.use_local_events({
        let focused = focused;
        let inputs = inputs;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE, KeyCode::Tab) => {
                        let mut f = focused.write();
                        *f = (*f + 1) % question_count;
                    }
                    (KeyModifiers::SHIFT, KeyCode::BackTab) | (KeyModifiers::NONE, KeyCode::BackTab) => {
                        let mut f = focused.write();
                        *f = f.checked_sub(1).unwrap_or(question_count - 1);
                    }
                    // Phase 8: Enter → on_submit, Esc → on_cancel
                    // 输入处理：可打印字符 + Backspace
                    (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                        let idx = *focused.read();
                        let mut input_vec = inputs.write();
                        input_vec[idx].push(c);
                    }
                    (KeyModifiers::NONE, KeyCode::Backspace) => {
                        let idx = *focused.read();
                        let mut input_vec = inputs.write();
                        input_vec[idx].pop();
                    }
                    _ => {}
                }
            }
        }
    });

    let focused_idx = *focused.read();
    let inputs_read = inputs.read();

    // 渲染问题列表 + 输入区域
    let display_lines: Vec<Line<'_>> = questions
        .iter()
        .enumerate()
        .flat_map(|(i, q)| {
            let is_focused = i == focused_idx;
            let answer = &inputs_read[i];

            vec![
                // 问题行
                if is_focused {
                    Line::from(format!("> {}", q.question)).fg(theme::THINKING).bold()
                } else {
                    Line::from(format!("  {}", q.question)).fg(theme::TEXT)
                },
                // 输入行
                if is_focused {
                    if answer.is_empty() {
                        Line::from(format!("  [{}] ", q.hint)).fg(theme::DIM)
                    } else {
                        Line::from(format!("  [ {} ]", answer)).fg(theme::SAGE)
                    }
                } else if answer.is_empty() {
                    Line::from("  (empty)").fg(theme::MUTED)
                } else {
                    Line::from(format!("  {}", answer)).fg(theme::MUTED)
                },
                // 空行分隔
                Line::from(""),
            ]
        })
        .collect();

    // 底部提示
    let footer = Line::from(" Tab: next | Shift+Tab: prev | Enter: submit | Esc: cancel ")
        .fg(theme::DIM);

    let mut all_lines = display_lines;
    all_lines.push(footer);

    let text_render = Paragraph::new(ratatui::text::Text::from(all_lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Question ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(12),
        ) {
            Text(text: text_render)
        }
    )
}
