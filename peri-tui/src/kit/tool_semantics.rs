//! Skill 与 TodoWrite 的用户语义展示逻辑。
//!
//! 只消费工具调用的结构化输入；不解析工具的原始输出，以避免将 SKILL.md 正文或
//! TodoWrite 的内部索引摘要泄漏到消息卡片中。

use crate::kit::tui_render_unit::{
    TuiSkillPresentation, TuiTodoChange, TuiTodoChangeKind, TuiTodoItem, TuiTodoPresentation,
    TuiTodoStatus, TuiToolPresentation,
};
use serde_json::Value;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TodoSnapshot {
    items: Vec<TuiTodoItem>,
}

impl TodoSnapshot {
    pub(crate) fn parse(input: &Value) -> Option<Self> {
        let todos = input.get("todos")?.as_array()?;
        let mut items = Vec::with_capacity(todos.len());
        for todo in todos {
            let content = todo.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let status = match todo.get("status")?.as_str()? {
                "pending" => TuiTodoStatus::Pending,
                "in_progress" => TuiTodoStatus::InProgress,
                "completed" => TuiTodoStatus::Completed,
                _ => return None,
            };
            let active_form = match todo.get("activeForm") {
                Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
                Some(Value::String(_)) | None => None,
                Some(_) => return None,
            };
            items.push(TuiTodoItem {
                content: content.to_string(),
                active_form,
                status,
            });
        }
        Some(Self { items })
    }

    pub(crate) fn items(&self) -> &[TuiTodoItem] {
        &self.items
    }
}

/// 从工具名称和结构化输入生成专属展示；未知或无效输入保留通用卡片。
pub(crate) fn presentation_for(
    tool_name: &str,
    raw_input: &Value,
    previous_todos: Option<&TodoSnapshot>,
) -> TuiToolPresentation {
    match tool_name {
        "Skill" | "SkillTool" => skill_presentation(raw_input),
        "TodoWrite" => todo_presentation(raw_input, previous_todos),
        _ => TuiToolPresentation::Generic,
    }
}

fn skill_presentation(raw_input: &Value) -> TuiToolPresentation {
    let name = raw_input
        .get("skill")
        .or_else(|| raw_input.get("skill_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown");

    TuiToolPresentation::Skill(TuiSkillPresentation {
        name: name.to_string(),
    })
}

fn todo_presentation(
    raw_input: &Value,
    previous_todos: Option<&TodoSnapshot>,
) -> TuiToolPresentation {
    let Some(current) = TodoSnapshot::parse(raw_input) else {
        return TuiToolPresentation::Generic;
    };
    let completed_count = current
        .items()
        .iter()
        .filter(|item| item.status == TuiTodoStatus::Completed)
        .count();
    let changes = match previous_todos {
        Some(previous) => diff_todos(previous.items(), current.items()),
        None => current
            .items()
            .iter()
            .map(|item| TuiTodoChange {
                kind: TuiTodoChangeKind::Added,
                content: item.content.clone(),
            })
            .collect(),
    };

    TuiToolPresentation::Todo(TuiTodoPresentation {
        current_items: current.items,
        changes,
        is_initial: previous_todos.is_none(),
        completed_count,
        total_count: raw_input
            .get("todos")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
    })
}

fn diff_todos(previous: &[TuiTodoItem], current: &[TuiTodoItem]) -> Vec<TuiTodoChange> {
    let mut unmatched_previous: VecDeque<usize> = (0..previous.len()).collect();
    let mut changes = Vec::new();

    for item in current {
        let match_position = unmatched_previous
            .iter()
            .position(|index| previous[*index].content == item.content);
        let Some(match_position) = match_position else {
            changes.push(change(TuiTodoChangeKind::Added, item));
            continue;
        };
        let previous_index = unmatched_previous
            .remove(match_position)
            .expect("matched index must exist");
        let previous_item = &previous[previous_index];
        if let Some(kind) = status_change_kind(previous_item.status, item.status) {
            changes.push(change(kind, item));
        }
        if previous_item.active_form != item.active_form {
            changes.push(change(TuiTodoChangeKind::ActiveFormUpdated, item));
        }
    }

    for previous_index in unmatched_previous {
        changes.push(change(
            TuiTodoChangeKind::Removed,
            &previous[previous_index],
        ));
    }
    changes
}

fn change(kind: TuiTodoChangeKind, item: &TuiTodoItem) -> TuiTodoChange {
    TuiTodoChange {
        kind,
        content: item.content.clone(),
    }
}

fn status_change_kind(
    previous: TuiTodoStatus,
    current: TuiTodoStatus,
) -> Option<TuiTodoChangeKind> {
    match (previous, current) {
        (previous, current) if previous == current => None,
        (_, TuiTodoStatus::Completed) => Some(TuiTodoChangeKind::Completed),
        (TuiTodoStatus::Completed, _) => Some(TuiTodoChangeKind::Reopened),
        (_, TuiTodoStatus::InProgress) => Some(TuiTodoChangeKind::Started),
        _ => Some(TuiTodoChangeKind::Reopened),
    }
}

#[cfg(test)]
#[path = "tool_semantics_test.rs"]
mod tests;
