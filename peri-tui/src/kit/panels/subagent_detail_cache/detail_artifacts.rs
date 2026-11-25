//! SubAgent 详情面板的 entry 副产物命中与动作接线。
//!
//! 面板的 `EntryRenderCache` 已经保留 GenUI 按钮与 Mermaid 围栏；这里只按面板
//! 内容区把逻辑行换算成屏幕命中，命中几何复用 `message_area::artifact_hits`，
//! 动作复用主区分发（GenUI [`dispatch_genui_action`] / Mermaid [`open_detail`]），
//! 不复制业务规则。Mermaid 详情由面板栈打开，不携带主区 slot 身份，关闭时不
//! 回写消息区焦点。

use std::sync::{Arc, Weak};

use super::detail_render_cache::DetailRenderCache;
use crate::kit::markdown::{MermaidFence, MermaidFenceKey};
use crate::kit::message_area::artifact_hits;
use crate::kit::message_area::dispatch_genui_action;
use peri_tui_components::genui::GenUiButtonInfo;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::text::Line;

#[derive(Default)]
pub(in super::super) struct DetailArtifacts {
    pub(in super::super) genui: Vec<DetailGenuiHit>,
    pub(in super::super) mermaid: Vec<DetailMermaidHit>,
}

/// GenUI 按钮的屏幕命中；`lines` 用于点击时校验缓存实例未重建。
#[derive(Clone)]
pub(in super::super) struct DetailGenuiHit {
    pub(in super::super) row: u16,
    pub(in super::super) x_start: u16,
    pub(in super::super) x_end: u16,
    pub(in super::super) slot_index: usize,
    pub(in super::super) button: GenUiButtonInfo,
    lines: Weak<Vec<Line<'static>>>,
}

/// Mermaid「查看图」动作行的屏幕命中。
#[derive(Clone)]
pub(in super::super) struct DetailMermaidHit {
    pub(in super::super) row: u16,
    pub(in super::super) x_start: u16,
    pub(in super::super) x_end: u16,
    pub(in super::super) key: MermaidFenceKey,
    pub(in super::super) fence: Arc<MermaidFence>,
    pub(in super::super) width: usize,
    slot_index: usize,
    lines: Weak<Vec<Line<'static>>>,
}

/// 由面板内容区与当前滚动偏移生成命中；与 `DetailRenderCache::draw` 同源几何。
pub(in super::super) fn collect(
    cache: &DetailRenderCache,
    area: Rect,
    top: usize,
) -> DetailArtifacts {
    let mut artifacts = DetailArtifacts::default();
    if cache.plain.is_some() || area.is_empty() {
        return artifacts;
    }
    let width = cache.grid.line_width().min(area.width);
    if width == 0 {
        return artifacts;
    }
    let header = cache.header.len();
    let bottom = top.saturating_add(area.height as usize).min(cache.height());
    let content_rows = top.saturating_sub(header)..bottom.saturating_sub(header);
    for index in cache.slots_for(content_rows) {
        let Some(slot) = cache.slots.get(index) else {
            continue;
        };
        if !slot.resident {
            continue;
        }
        let base = header.saturating_add(cache.prefix[index]);
        let lines = slot.entry.lines();
        for button in slot.entry.genui_buttons() {
            for (visual, start, end) in
                artifact_hits::genui_button_rows(button, &slot.entry, &slot.wrap_map, width)
            {
                let Some(relative) = base.saturating_add(visual).checked_sub(top) else {
                    continue;
                };
                if relative >= area.height as usize {
                    continue;
                }
                artifacts.genui.push(DetailGenuiHit {
                    row: area.y.saturating_add(relative as u16),
                    x_start: area.x.saturating_add(start),
                    x_end: area.x.saturating_add(end),
                    slot_index: index,
                    button: button.clone(),
                    lines: Arc::downgrade(lines),
                });
            }
        }
        let (x_start, x_end) = artifact_hits::mermaid_action_columns(cache.grid, area);
        for span in slot.entry.mermaid_fences() {
            let Some(visual) = artifact_hits::mermaid_action_visual(span, &slot.wrap_map) else {
                continue;
            };
            let Some(relative) = base.saturating_add(visual).checked_sub(top) else {
                continue;
            };
            if relative >= area.height as usize {
                continue;
            }
            artifacts.mermaid.push(DetailMermaidHit {
                row: area.y.saturating_add(relative as u16),
                x_start,
                x_end,
                key: span.key.clone(),
                fence: Arc::clone(&span.fence),
                width: span.width,
                slot_index: index,
                lines: Arc::downgrade(lines),
            });
        }
    }
    artifacts
}

/// 面板点击解析结果——先释放缓存读锁，再执行动作。
pub(in super::super) enum DetailClick {
    GenUi(GenUiButtonInfo),
    Mermaid {
        key: MermaidFenceKey,
        fence: Arc<MermaidFence>,
        width: usize,
    },
    /// 命中旧帧热区（缓存已重建）——认领点击但不执行动作。
    Stale,
    None,
}

/// 面板点击：先匹配副产物热区（与主区同优先级：按钮先于折叠头），未命中
/// 返回 [`DetailClick::None`] 交回面板的折叠头/滚动处理。
///
/// 命中区域总是被认领（与主区 `dispatch_click` 一致）；只有缓存实例仍指向同一
/// 行快照时才产生动作，避免 resize/重排后点击落到旧按钮。
pub(in super::super) fn resolve_click(
    cache: &DetailRenderCache,
    artifacts: &DetailArtifacts,
    row: u16,
    column: u16,
) -> DetailClick {
    if let Some(hit) = artifacts
        .genui
        .iter()
        .find(|hit| row == hit.row && column >= hit.x_start && column < hit.x_end)
    {
        return if genui_current(cache, hit) {
            DetailClick::GenUi(hit.button.clone())
        } else {
            DetailClick::Stale
        };
    }
    if let Some(hit) = artifacts
        .mermaid
        .iter()
        .find(|hit| row == hit.row && column >= hit.x_start && column < hit.x_end)
    {
        return if mermaid_current(cache, hit) {
            DetailClick::Mermaid {
                key: hit.key.clone(),
                fence: Arc::clone(&hit.fence),
                width: hit.width,
            }
        } else {
            DetailClick::Stale
        };
    }
    DetailClick::None
}

/// 执行已解析的点击；返回面板是否应消费该事件。
pub(in super::super) fn apply_click(click: DetailClick) -> bool {
    match click {
        DetailClick::GenUi(button) => dispatch_genui_action(&button),
        DetailClick::Mermaid { key, fence, width } => {
            // 面板打开的图不绑定主区 slot/vm_hash：Esc 关闭后由面板栈回到面板，
            // 不把消息区焦点改写到面板条目。
            crate::kit::mermaid::open_detail(key, &fence, width, None, None);
        }
        DetailClick::Stale => return true,
        DetailClick::None => return false,
    }
    true
}

fn genui_current(cache: &DetailRenderCache, hit: &DetailGenuiHit) -> bool {
    let Some(slot) = cache.slots.get(hit.slot_index) else {
        return false;
    };
    if !slot.resident || !same_lines(&hit.lines, &slot.entry) {
        return false;
    }
    slot.entry
        .genui_buttons()
        .iter()
        .any(|button| button == &hit.button)
}

fn mermaid_current(cache: &DetailRenderCache, hit: &DetailMermaidHit) -> bool {
    let Some(slot) = cache.slots.get(hit.slot_index) else {
        return false;
    };
    if !slot.resident || !same_lines(&hit.lines, &slot.entry) {
        return false;
    }
    slot.entry.mermaid_fences().iter().any(|span| {
        span.key == hit.key
            && span.fence.source == hit.fence.source
            && span.fence.raw == hit.fence.raw
    })
}

fn same_lines(
    lines: &Weak<Vec<Line<'static>>>,
    entry: &crate::kit::entry_render_cache::EntryRenderCache,
) -> bool {
    lines
        .upgrade()
        .is_some_and(|lines| Arc::ptr_eq(&lines, entry.lines()))
}
