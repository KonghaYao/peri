//! Tasks 面板的三段列表视口——内容区行数 → 行表（绘制、鼠标命中与取消目标解析的唯一推导处）。
//!
//! [Why] 本面板把后台任务、Cron 任务与 SubAgent 拼在同一个连续选中下标空间里（段顺序
//! 即下标顺序）。行结构不是「等行高项区」：段标题只在该段有可见项时出现，段后空行只
//! 跟在部分段之后，空态另有 3 行。`shared::list::ListMetrics`（header / item / footer
//! 各一个固定行数）与 `panel_mouse::hit_row`（按等行高项区反推行号）都表达不了这种交错，
//! 因此这里以**行表**为唯一产物：绘制逐行渲染行表、鼠标按行号查行表、取消目标按行表里
//! 的全量下标解析——绘制与命中不可能各算一遍。
//!
//! 该模块不引入面板级状态，`TasksViewport` 是每帧由「内容区行数 + 段项数 + 选中下标」
//! 推导出的值对象；首帧区域未测量时的内容行数由调用方按 `shared::list::content_rows`
//! 预测。行高：每行恰好 1 行（段标题、空行、任务行、摘要、提示均单行），故没有行高参数。

use ratatui_kit::crossterm::event::MouseEvent;
use ratatui_kit::ratatui::layout::Rect;

use crate::kit::atoms::BgTaskEntry;
use crate::kit::list_nav::{clamp_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::left_down;

/// 段顺序——即选中下标空间（与 `selected`）的排列顺序，绘制顺序同此。
pub(super) const SEGMENTS: [TaskSegment; 3] = [
    TaskSegment::Background,
    TaskSegment::Cron,
    TaskSegment::SubAgent,
];

/// 任务来源分段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskSegment {
    Background,
    Cron,
    SubAgent,
}

/// 各段项数——全量下标空间的段边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct SegmentCounts {
    pub background: usize,
    pub cron: usize,
    pub subagent: usize,
}

impl SegmentCounts {
    /// 三段总项数（= 选中下标空间长度）。
    pub(super) fn total(self) -> usize {
        self.background + self.cron + self.subagent
    }

    /// 段在全量下标空间中的起点。
    fn base(self, segment: TaskSegment) -> usize {
        match segment {
            TaskSegment::Background => 0,
            TaskSegment::Cron => self.background,
            TaskSegment::SubAgent => self.background + self.cron,
        }
    }

    /// 段长。
    fn len(self, segment: TaskSegment) -> usize {
        match segment {
            TaskSegment::Background => self.background,
            TaskSegment::Cron => self.cron,
            TaskSegment::SubAgent => self.subagent,
        }
    }
}

/// 一帧内容区中的一行。非 `Item` 行不接受鼠标命中，也不提供取消目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TasksRow {
    /// 摘要行（总数 + 三段明细）
    Summary,
    /// 段标题行
    SectionHeader(TaskSegment),
    /// 任务行——值是全量下标（三段连续编号，与选中态、鼠标命中同一坐标）
    Item(usize),
    /// 空态标题（列表为空）
    EmptyTitle,
    /// 空态引导：Cron 任务如何创建
    EmptyHintCron,
    /// 空态引导：SubAgent 如何创建
    EmptyHintSubagent,
    /// 空行（段后、末尾）
    Blank,
    /// 底部导航提示
    NavHint,
}

/// 取消目标身份——取自渲染帧的行表与后台任务快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CancelTarget {
    pub task_id: String,
    pub kind: String,
}

/// 行动作解析结果——三种结果都有明确的用户可见后果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CancelAction {
    /// 该行是后台任务行，且身份在当帧快照里 → 取消它
    Cancel(CancelTarget),
    /// 该行渲染成后台任务行，但身份已不在当帧快照里（渲染与按键之间列表刷新）
    /// → 不发请求，提示用户并保持面板打开
    TargetGone,
    /// 该行本就不指向后台任务（Cron / SubAgent 行、空态）→ 沿用 Enter::close
    NotCancelable,
}

/// 一帧视口：行表 + 本帧段边界 + 本帧收敛后的选中下标。
#[derive(Debug, Clone)]
pub(super) struct TasksViewport {
    rows: Vec<TasksRow>,
    counts: SegmentCounts,
    selected: usize,
}

impl TasksViewport {
    /// 由面板内容区行数（不含上下边框）推导本帧视口。
    ///
    /// `selected` 超出当帧列表长度时按 `clamp_selection` 收敛（与 `ListViewport`
    /// 同语义）：列表刷新缩短后不会留下越界选择，渲染与事件都拿得到高亮行。
    pub(super) fn for_content_rows(
        content_rows: u16,
        counts: SegmentCounts,
        selected: usize,
    ) -> Self {
        let selected = clamp_selection(selected, counts.total());
        let capacity = (content_rows as usize).saturating_sub(fixed_rows(counts));
        let scroll_start = scroll_start_for_selected(selected, counts.total(), capacity);
        let mut rows = build_rows(counts, capacity, scroll_start);
        // 面板矮于固定行时按预算截行：绘制本来也会被裁掉，截断保证行表不会给出
        // 屏幕外的命中（`panel_mouse::hit_row` 同款「越界不可点」语义）。
        rows.truncate(content_rows as usize);
        Self {
            rows,
            counts,
            selected,
        }
    }

    /// 本帧行表——绘制按它逐行渲染。
    pub(super) fn rows(&self) -> &[TasksRow] {
        &self.rows
    }

    /// 本帧段边界。
    pub(super) fn counts(&self) -> SegmentCounts {
        self.counts
    }

    /// 本帧收敛后的选中下标。
    pub(super) fn selected(&self) -> usize {
        self.selected
    }

    /// 选中项的全量下标——必须在当帧行表里（= 用户看得到这一行）。
    ///
    /// 列表为空、或面板矮到行表放不下任何任务行时返回 None：Enter 不得取消屏幕上
    /// 没有任何痕迹的任务。
    pub(super) fn selected_item(&self) -> Option<usize> {
        self.rows.iter().find_map(|row| match row {
            TasksRow::Item(index) if *index == self.selected => Some(*index),
            _ => None,
        })
    }

    /// 行表里 `row` 行对应的全量下标；非任务行或越界 → None。
    pub(super) fn item_at_row(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            TasksRow::Item(index) => Some(*index),
            _ => None,
        }
    }

    /// 鼠标命中（click as enter）：行号按行表反推全量下标，与绘制同源。
    ///
    /// [Why] 不复用 `panel_mouse::hit_item`：它按单一等行高布局（`ListLayout`）反推
    /// 行号，而本面板的段标题与段间空行把任务行切成三段，同一算式会把空行算成任务行。
    /// 行表长度已被内容行预算截断，因此行号越界（含上下边框）自然落空。
    pub(super) fn hit_item(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        let (row, _column) = left_down(mouse)?;
        // 顶部边框行不可点
        let visual = row.checked_sub(area.y.saturating_add(1))?;
        self.item_at_row(visual as usize)
    }

    /// 某一行的取消动作。
    ///
    /// [Why] 段边界取自本帧 `counts`、身份取自本帧 `bg_tasks` 快照（事件闭包随渲染帧
    /// 重建），因此「下标 < 后台段长度」就是「用户看到的是可取消的后台任务行」。
    /// 旧实现在按键帧重读 atom 后按下标取 `task_id`：渲染与按键之间列表一刷新，
    /// 取消就会发给用户没看到的另一个任务。
    pub(super) fn cancel_action(&self, index: usize, bg_tasks: &[BgTaskEntry]) -> CancelAction {
        if index >= self.counts.background {
            return CancelAction::NotCancelable;
        }
        match bg_tasks.get(index) {
            Some(task) => CancelAction::Cancel(CancelTarget {
                task_id: task.task_id.clone(),
                kind: task.kind.clone(),
            }),
            None => CancelAction::TargetGone,
        }
    }
}

/// 固定行开销：摘要 + 末尾空行与导航提示，加上按段存在性变化的段标题与段间空行
/// （空态另有 3 行）。
///
/// [Why] 可见项数 = 内容区行数 − 固定行，而固定行不是常量：段为空时既无标题也无
/// 段间空行，SubAgent 段在末尾没有段后空行。按存在性逐段累加才能得到真实的项区容量。
fn fixed_rows(counts: SegmentCounts) -> usize {
    // 摘要 + 空行，末尾空行 + 导航提示
    let mut rows = 4;
    if counts.total() == 0 {
        // 空态：标题 + 两条引导
        return rows + 3;
    }
    for segment in SEGMENTS {
        if counts.len(segment) == 0 {
            continue;
        }
        // 段标题；后台 / Cron 段后有段间空行，末尾的 SubAgent 段没有
        rows += if segment == TaskSegment::SubAgent {
            1
        } else {
            2
        };
    }
    rows
}

/// 本帧行表：段标题与段间空行只在「该段有可见项」时出现——绘制与命中都只认这张表。
fn build_rows(counts: SegmentCounts, capacity: usize, scroll_start: usize) -> Vec<TasksRow> {
    let mut rows: Vec<TasksRow> = vec![TasksRow::Summary, TasksRow::Blank];
    if counts.total() == 0 {
        rows.push(TasksRow::EmptyTitle);
        rows.push(TasksRow::EmptyHintCron);
        rows.push(TasksRow::EmptyHintSubagent);
    } else {
        let window_end = scroll_start.saturating_add(capacity).min(counts.total());
        // 本帧各段的可见范围：段被窗口整体跳过时既不渲染标题也不留空行，
        // 因此段间空行只跟在「后面还有段」的段之后。
        let visible: Vec<(TaskSegment, usize, usize)> = SEGMENTS
            .iter()
            .map(|segment| {
                let base = counts.base(*segment);
                let start = base.max(scroll_start);
                let end = (base + counts.len(*segment)).min(window_end);
                (*segment, start, end)
            })
            .filter(|(_, start, end)| start < end)
            .collect();
        let last_slot = visible.len().saturating_sub(1);
        for (slot, (segment, start, end)) in visible.into_iter().enumerate() {
            rows.push(TasksRow::SectionHeader(segment));
            rows.extend((start..end).map(TasksRow::Item));
            if slot != last_slot {
                rows.push(TasksRow::Blank);
            }
        }
    }
    rows.push(TasksRow::Blank);
    rows.push(TasksRow::NavHint);
    rows
}
