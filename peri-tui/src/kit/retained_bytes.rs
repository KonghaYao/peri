#![allow(dead_code)] // S6.1 统一计量 API 在释放/诊断路径逐步接入

//! S6.1 统一正文计量：body 实际字节、共享 `Arc` 去重、深复制可识别。
//!
//! 口径：字符串/容器 `capacity`、结构体 `size_of`、`Arc<T>` 只按指针计一次。
//! 这是 retained 账目，不是 RSS 证据。

use std::collections::HashMap;
use std::sync::Arc;

use ratatui_kit::ratatui::text::Line;

use super::tui_render_unit::{
    TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDivider, TuiRenderUnit,
    TuiSubAgentGroup, TuiSystemNote, TuiSystemReminder, TuiTodoSummary, TuiToolCard,
    TuiToolPresentation, TuiUserBubble,
};

/// `body` = 已使用正文字节；`reserved` = `String`/容器的 capacity 下界。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyReserved {
    pub body: usize,
    pub reserved: usize,
}

impl std::ops::Add for BodyReserved {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            body: self.body + rhs.body,
            reserved: self.reserved + rhs.reserved,
        }
    }
}

impl std::ops::AddAssign for BodyReserved {
    fn add_assign(&mut self, rhs: Self) {
        self.body += rhs.body;
        self.reserved += rhs.reserved;
    }
}

pub fn string_body_reserved(value: &String) -> BodyReserved {
    BodyReserved {
        body: value.len(),
        reserved: value.capacity(),
    }
}

pub fn option_string_body_reserved(value: &Option<String>) -> BodyReserved {
    value.as_ref().map(string_body_reserved).unwrap_or_default()
}

/// 结构化 JSON 的逻辑字节（不分配序列化临时体）。
pub fn json_value_bytes(value: &serde_json::Value) -> BodyReserved {
    use serde_json::Value;
    match value {
        Value::Null => BodyReserved {
            body: 4,
            reserved: 4,
        },
        Value::Bool(true) => BodyReserved {
            body: 4,
            reserved: 4,
        },
        Value::Bool(false) => BodyReserved {
            body: 5,
            reserved: 5,
        },
        Value::Number(_) => BodyReserved {
            body: 8,
            reserved: 8,
        },
        Value::String(text) => BodyReserved {
            body: text.len() + 2,
            reserved: text.capacity() + 2,
        },
        Value::Array(items) => {
            let mut bytes = BodyReserved {
                body: 2 + items.len(),
                reserved: 2 + items.len(),
            };
            for item in items {
                bytes += json_value_bytes(item);
            }
            bytes
        }
        Value::Object(map) => {
            let mut bytes = BodyReserved {
                body: 2 + map.len(),
                reserved: 2 + map.len(),
            };
            for (key, item) in map {
                let item = json_value_bytes(item);
                bytes.body += key.len() + 4 + item.body;
                bytes.reserved += key.len() + 4 + item.reserved;
            }
            bytes
        }
    }
}

pub fn presentation_bytes(presentation: &TuiToolPresentation) -> BodyReserved {
    match presentation {
        TuiToolPresentation::Generic => BodyReserved::default(),
        TuiToolPresentation::Skill(skill) => string_body_reserved(&skill.name),
        TuiToolPresentation::Todo(todo) => {
            let mut bytes = BodyReserved::default();
            for item in &todo.current_items {
                bytes += string_body_reserved(&item.content);
                bytes += option_string_body_reserved(&item.active_form);
            }
            for change in &todo.changes {
                bytes += string_body_reserved(&change.content);
            }
            bytes
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SeenAllocations {
    seen: HashMap<usize, usize>,
}

impl SeenAllocations {
    pub fn account_arc<T>(&mut self, arc: &Arc<T>, bytes: usize) -> usize {
        self.account_ptr(Arc::as_ptr(arc) as usize, bytes)
    }

    pub fn account_ptr(&mut self, ptr: usize, bytes: usize) -> usize {
        match self.seen.entry(ptr) {
            std::collections::hash_map::Entry::Occupied(_) => 0,
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(bytes);
                bytes
            }
        }
    }

    pub fn forget(&mut self, ptr: usize) -> bool {
        self.seen.remove(&ptr).is_some()
    }

    pub fn bytes(&self) -> usize {
        self.seen.values().copied().sum()
    }

    pub fn allocations(&self) -> usize {
        self.seen.len()
    }
}

pub fn retained_str_bytes(value: &str) -> usize {
    value.len()
}

pub fn retained_vec_bytes<T>(values: &[T]) -> usize {
    std::mem::size_of_val(values)
}

pub fn retained_lines_bytes(lines: &[Line<'static>]) -> usize {
    lines
        .iter()
        .map(|line| {
            std::mem::size_of::<Line<'static>>()
                + line.spans.capacity()
                    * std::mem::size_of::<ratatui_kit::ratatui::text::Span<'static>>()
                + line
                    .spans
                    .iter()
                    .map(|span| span.content.len())
                    .sum::<usize>()
        })
        .sum()
}

pub fn retained_arc_lines_bytes(
    arc: &Arc<Vec<Line<'static>>>,
    seen: &mut SeenAllocations,
) -> usize {
    if arc.is_empty() && arc.capacity() == 0 {
        return 0;
    }
    let total = std::mem::size_of::<Vec<Line<'static>>>() + retained_lines_bytes(arc);
    seen.account_arc(arc, total)
}

pub fn retained_unit_bytes(unit: &TuiRenderUnit, seen: &mut SeenAllocations) -> usize {
    match unit {
        TuiRenderUnit::TuiAssistantBubble(bubble) => retained_bubble_bytes(bubble, seen),
        TuiRenderUnit::TuiUserBubble(bubble) => retained_user_bytes(bubble),
        TuiRenderUnit::TuiToolCard(card) => retained_tool_bytes(card),
        TuiRenderUnit::TuiSystemNote(note) => retained_note_bytes(note),
        TuiRenderUnit::TuiSystemReminder(reminder) => retained_reminder_bytes(reminder),
        TuiRenderUnit::TuiSubAgentGroup(group) => retained_subagent_group_bytes(group, seen),
        TuiRenderUnit::TuiCollapsedGroup(group) => retained_collapsed_group_bytes(group, seen),
        TuiRenderUnit::TuiDivider(divider) => retained_divider_bytes(divider),
        TuiRenderUnit::TuiAskUserBlock(block) => retained_ask_user_bytes(block),
        TuiRenderUnit::TuiTodoSummary(summary) => retained_todo_bytes(summary),
    }
}

pub fn retained_units_bytes<'a>(
    units: impl IntoIterator<Item = &'a TuiRenderUnit>,
    seen: &mut SeenAllocations,
) -> usize {
    units
        .into_iter()
        .map(|unit| retained_unit_bytes(unit, seen))
        .sum()
}

pub fn retained_bubble_bytes(
    bubble: &Arc<TuiAssistantBubble>,
    seen: &mut SeenAllocations,
) -> usize {
    let payload = std::mem::size_of::<TuiAssistantBubble>()
        + bubble.text.capacity()
        + bubble.message_id.as_ref().map_or(0, String::capacity)
        + bubble.reasoning.as_ref().map_or(0, |reasoning| {
            std::mem::size_of_val(reasoning) + reasoning.text.capacity()
        });
    seen.account_arc(bubble, payload)
}

pub fn retained_user_bytes(bubble: &TuiUserBubble) -> usize {
    std::mem::size_of::<TuiUserBubble>()
        + bubble.text.capacity()
        + retained_vec_bytes(&bubble.attachments)
        + bubble.source.as_ref().map_or(0, String::capacity)
}

pub fn retained_tool_bytes(card: &TuiToolCard) -> usize {
    std::mem::size_of::<TuiToolCard>()
        + card.tool_id.capacity()
        + card.tool_name.capacity()
        + card.input_summary.capacity()
        + card.output_summary.capacity()
        + card.diff.as_ref().map_or(0, |diff| {
            std::mem::size_of_val(diff)
                + diff.path.capacity()
                + retained_vec_bytes(&diff.hunks)
                + diff
                    .hunks
                    .iter()
                    .map(|hunk| {
                        hunk.old_range.capacity()
                            + hunk.new_range.capacity()
                            + retained_vec_bytes(&hunk.lines)
                    })
                    .sum::<usize>()
        })
}

pub fn retained_note_bytes(note: &TuiSystemNote) -> usize {
    std::mem::size_of::<TuiSystemNote>() + note.text.capacity()
}

pub fn retained_reminder_bytes(reminder: &TuiSystemReminder) -> usize {
    std::mem::size_of::<TuiSystemReminder>()
        + reminder.summary.capacity()
        + reminder.body.capacity()
        + reminder.category.capacity()
        + reminder.source.capacity()
        + reminder
            .relay
            .as_ref()
            .map_or(0, |relay| relay.agent_id.capacity())
}

pub fn retained_divider_bytes(divider: &TuiDivider) -> usize {
    std::mem::size_of::<TuiDivider>() + divider.label.as_ref().map_or(0, String::capacity)
}

pub fn retained_todo_bytes(summary: &TuiTodoSummary) -> usize {
    std::mem::size_of::<TuiTodoSummary>() + summary.text.capacity()
}

pub fn retained_ask_user_bytes(block: &TuiAskUserBlock) -> usize {
    std::mem::size_of::<TuiAskUserBlock>()
        + retained_vec_bytes(&block.items)
        + block
            .items
            .iter()
            .map(|item| item.header.capacity() + item.answer.capacity())
            .sum::<usize>()
        + block.verb.capacity()
        + block.question.capacity()
        + retained_vec_bytes(&block.options)
        + block.options.iter().map(String::capacity).sum::<usize>()
        + block.result.as_ref().map_or(0, String::capacity)
        + block.request_id.as_ref().map_or(0, String::capacity)
}

pub fn retained_collapsed_group_bytes(
    group: &TuiCollapsedGroup,
    seen: &mut SeenAllocations,
) -> usize {
    std::mem::size_of::<TuiCollapsedGroup>()
        + group.title.capacity()
        + retained_vec_bytes(&group.view_models)
        + retained_units_bytes(group.view_models.iter(), seen)
}

pub fn retained_subagent_group_bytes(
    group: &TuiSubAgentGroup,
    seen: &mut SeenAllocations,
) -> usize {
    std::mem::size_of::<TuiSubAgentGroup>()
        + group.instance_id.capacity()
        + group.agent_id.capacity()
        + group.agent_name.capacity()
        + group.error_reason.as_ref().map_or(0, String::capacity)
        + retained_units_bytes(group.view_models.iter(), seen)
}

/// 组内成员的“深复制正文”字节：不按共享 Arc 去重，用于识别 `Vec<TuiRenderUnit>`
/// 复制带来的额外持有（与 `retained_*_group_bytes` 的共享计量对照）。
pub fn group_deep_copy_bytes(group: &TuiCollapsedGroup) -> usize {
    let mut seen = SeenAllocations::default();
    group
        .view_models
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => {
                // 共享 Arc 也按“若复制正文”口径计一次完整正文。
                std::mem::size_of::<TuiAssistantBubble>() + bubble.text.capacity()
            }
            other => retained_unit_bytes(other, &mut seen),
        })
        .sum()
}

#[cfg(test)]
#[path = "retained_bytes_test.rs"]
mod tests;
