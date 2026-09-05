//! BgTask 底栏行点击：命中几何、kind 路由与 D4 遮挡谓词（纯函数 + handler 接线）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    BgDisplayEntry, NOTIFICATION, Notification, SELECTED_BG_TASK_ID, SELECTED_SUBAGENT_ID,
    SELECTED_WORKFLOW_RUN_ID,
};
use crate::kit::bg_task_identity::resolve_subagent_id_for_display;
use crate::kit::panel_registry::open_panel;
use ratatui_kit::ratatui::layout::Rect;
use std::time::{Duration, Instant};

/// 与 `bg_task_area` 渲染层一致的完成后保留时长（秒）。
pub const DONE_KEEP_SECS: u64 = 3;

const UNKNOWN_KIND_NOTIFY_SECS: u64 = 2;

/// 单行热区（屏幕绝对坐标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BgTaskLineHit {
    pub row: u16,
    pub task_id: String,
    pub kind: String,
    pub sorted_index: usize,
}

/// 按 kind 路由结果（纯函数输出；由 handler 写入 atom 并 `open_panel`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BgTaskClickRoute {
    SubAgent { subagent_id: String },
    Shell { task_id: String },
    Workflow { run_id: String },
    UnknownKind,
}

fn safe_elapsed(later: Instant, earlier: Instant) -> Duration {
    if later >= earlier {
        later.duration_since(earlier)
    } else {
        Duration::ZERO
    }
}

/// 与渲染相同的 active 过滤（活跃 + 3s 缓冲）。
pub fn visible_bg_display_entries(
    entries: &[BgDisplayEntry],
    now: Instant,
) -> Vec<&BgDisplayEntry> {
    entries
        .iter()
        .filter(|e| {
            e.is_active
                || e.completed_at
                    .is_none_or(|t| safe_elapsed(now, t).as_secs() < DONE_KEEP_SECS)
        })
        .collect()
}

/// 与渲染相同的排序：活跃在前。
pub fn sort_bg_display_rows(mut active: Vec<&BgDisplayEntry>) -> Vec<&BgDisplayEntry> {
    active.sort_by_key(|e| (!e.is_active, e.completed_at));
    active
}

/// 由组件 area 与 sorted 行列表构建每行整行热区。
pub fn build_bg_task_line_hits(area: Rect, sorted: &[&BgDisplayEntry]) -> Vec<BgTaskLineHit> {
    sorted
        .iter()
        .enumerate()
        .map(|(i, entry)| BgTaskLineHit {
            row: area.y.saturating_add(i as u16),
            task_id: entry.id.clone(),
            kind: entry.agent_type.clone(),
            sorted_index: i,
        })
        .collect()
}

/// 命中测试：列须在 area 宽度内，行须匹配某一热区行（整行可点）。
pub fn hit_test_bg_task_line(
    hits: &[BgTaskLineHit],
    area: Rect,
    column: u16,
    row: u16,
) -> Option<&BgTaskLineHit> {
    let right = area.x.saturating_add(area.width);
    if column < area.x || column >= right {
        return None;
    }
    hits.iter().find(|h| h.row == row)
}

/// 按 `agent_type` 路由；agent 行用 identity 解析 subagent id。
pub fn route_bg_task_click(entry: &BgDisplayEntry) -> BgTaskClickRoute {
    match entry.agent_type.as_str() {
        "agent" => {
            let subagent_id =
                resolve_subagent_id_for_display(entry).unwrap_or_else(|| entry.id.clone());
            BgTaskClickRoute::SubAgent { subagent_id }
        }
        "shell" => BgTaskClickRoute::Shell {
            task_id: entry.id.clone(),
        },
        "workflow" => BgTaskClickRoute::Workflow {
            run_id: entry.id.clone(),
        },
        _ => BgTaskClickRoute::UnknownKind,
    }
}

/// 将路由结果落到选中 atom 并打开面板（未知 kind 仅通知，不打开）。
pub fn apply_bg_task_click_route(route: BgTaskClickRoute) {
    match route {
        BgTaskClickRoute::SubAgent { subagent_id } => {
            *SELECTED_SUBAGENT_ID.state().write() = Some(subagent_id);
            open_panel(PanelKind::SubAgentDetail);
        }
        BgTaskClickRoute::Shell { task_id } => {
            *SELECTED_BG_TASK_ID.state().write() = Some(task_id);
            open_panel(PanelKind::ShellDetail);
        }
        BgTaskClickRoute::Workflow { run_id } => {
            *SELECTED_WORKFLOW_RUN_ID.state().write() = Some(run_id);
            open_panel(PanelKind::Workflow);
        }
        BgTaskClickRoute::UnknownKind => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("bg-task-unknown-kind"),
                until: Instant::now() + Duration::from_secs(UNKNOWN_KIND_NOTIFY_SECS),
            });
        }
    }
}

/// 在 sorted 索引处取条目并路由（handler 用）。
pub fn route_bg_task_click_at_index(
    sorted: &[&BgDisplayEntry],
    sorted_index: usize,
) -> Option<BgTaskClickRoute> {
    sorted
        .get(sorted_index)
        .map(|entry| route_bg_task_click(entry))
}

#[cfg(test)]
#[path = "bg_task_click_test.rs"]
mod tests;
