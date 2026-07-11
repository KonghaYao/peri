//! MessageArea 渲染缓存预处理桥。
//!
//! 独立 tokio task 监听 ACP 流式事件与宽度变化，预计算每条 TuiRenderUnit 的
//! `Vec<Line<'static>>` 与可视高度，并写入 `RENDER_CACHE` atom。
//!
//! 检测逻辑：比较 `ViewModelsSnapshot.generation`（替代 Arc::as_ptr），
//! 变化时触发全量/增量重建。

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::atoms::{ACP_STATE, RENDER_CACHE, VIEW_MODELS};
use crate::kit::markdown::MarkdownSegment;
use crate::kit::view_render;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VmKey {
    Item(usize),
}

/// 预渲染条目：文本行或表格数据。
#[derive(Debug, Clone)]
pub enum RenderedEntry {
    /// 纯文本行（已转为 `Arc<[Line]>`）。
    Text {
        height: usize,
        lines: Arc<[Line<'static>]>,
    },
    /// 表格数据，由 message_area 用 ratatui-kit Table 组件渲染。
    Table {
        height: usize,
        data: Arc<crate::kit::markdown::TableData>,
    },
}

impl RenderedEntry {
    pub fn height(&self) -> usize {
        match self {
            RenderedEntry::Text { height, .. } | RenderedEntry::Table { height, .. } => *height,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RenderCache {
    pub entries: Vec<(VmKey, RenderedEntry)>,
    pub cumulative_heights: Vec<usize>,
}

/// 每条逻辑行的 wrap 信息——用于视口裁剪的二分查找
#[derive(Debug, Clone)]
pub struct WrappedLineInfo {
    pub line_idx: usize,
    pub visual_row: u16,
    pub visual_height: u16,
}

pub fn spawn_render_bridge(
    mut rx: UnboundedReceiver<AcpEventWithEpoch>,
    mut resize_rx: UnboundedReceiver<u16>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_resize_width: usize = 80;
        let mut last_generation: u64 = 0;
        let mut last_reset_counter: u64 = 0;
        let mut cache = RenderCache::default();
        let mut msg_hashes: Vec<u64> = Vec::new();

        // H4: resize 去抖——记录上次 rebuild_all 时间 + 暂存被节流的宽度。
        // 终端 resize 会触发 burst 风暴（每帧一个事件），节流到 80ms 避免重建抖动。
        let mut last_rebuild_at: Option<std::time::Instant> = None;
        let mut pending_resize: Option<usize> = None;

        // 每秒轮询 VIEW_MODELS atom——检测 generation 变化（如 running Bash 计时器）
        let mut poll_interval = tokio::time::interval(std::time::Duration::from_secs(1));
        poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = poll_interval.tick() => {
                    // H4: 兑现被节流的 resize（距上次 rebuild >= 80ms 才补做）
                    if let Some(w) = pending_resize
                        && last_rebuild_at
                            .map(|t| t.elapsed() >= std::time::Duration::from_millis(80))
                            .unwrap_or(true)
                        && w != last_resize_width
                    {
                        last_resize_width = w;
                        let snapshot =
                            rebuild_all(last_resize_width, &mut cache, &mut last_generation).await;
                        msg_hashes = extract_hashes_from_im(&snapshot.items);
                        last_rebuild_at = Some(std::time::Instant::now());
                        pending_resize = None;
                    }

                    let snapshot = VIEW_MODELS.state().read().clone();
                    if snapshot.generation != last_generation {
                        // 活跃流式期间跳过 poll tick 全量重建。
                        // ACP 事件已通过 event 分支增量更新 RENDER_CACHE，
                        // 并发全量重建的 yield 点可能与事件分支产生竞态，
                        // 导致缓存中间态被消息区读到并渲染为空白帧。
                        let is_loading = ACP_STATE.state().read().is_loading;
                        if is_loading {
                            last_generation = snapshot.generation;
                            continue;
                        }
                        rebuild_entries(&mut cache.entries, &snapshot.items, last_resize_width).await;
                        msg_hashes = extract_hashes_from_im(&snapshot.items);
                        rebuild_cumulative_heights(&mut cache);
                        *RENDER_CACHE.state().write() = cache.clone();
                        last_generation = snapshot.generation;
                    }
                }
                Some(new_width) = resize_rx.recv() => {
                    // H4: 排空 burst，取最后一个宽度（合并 resize 风暴）
                    let mut w = usize::from(new_width).max(1);
                    while let Ok(next) = resize_rx.try_recv() {
                        w = usize::from(next).max(1);
                    }
                    if w == last_resize_width {
                        continue;
                    }
                    // 节流：距上次 rebuild >= 80ms 才立即重建，否则暂存等下次 poll/事件补做
                    let ready = last_rebuild_at
                        .map(|t| t.elapsed() >= std::time::Duration::from_millis(80))
                        .unwrap_or(true);
                    if ready {
                        last_resize_width = w;
                        let snapshot =
                            rebuild_all(last_resize_width, &mut cache, &mut last_generation).await;
                        msg_hashes = extract_hashes_from_im(&snapshot.items);
                        last_rebuild_at = Some(std::time::Instant::now());
                        pending_resize = None;
                    } else {
                        pending_resize = Some(w);
                    }
                }
                Some(_epoch_event) = rx.recv() => {
                    // BRIDGE_RESET_COUNTER 变更 → 清空缓存
                    let counter = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
                    let did_clear = if counter != last_reset_counter {
                        last_reset_counter = counter;
                        cache = RenderCache::default();
                        msg_hashes.clear();
                        last_generation = 0;
                        // H4: reset 时一并清空节流状态，避免 reset 后触发旧宽度的 rebuild
                        pending_resize = None;
                        last_rebuild_at = None;
                        *RENDER_CACHE.state().write() = cache.clone();
                        info!(counter, "render_bridge: cache cleared by BRIDGE_RESET_COUNTER");
                        true
                    } else {
                        false
                    };

                    let Some(snapshot) = read_ready_snapshot(last_generation).await else {
                        info!("render_bridge: event dropped (generation unchanged after 200 yields)");
                        continue;
                    };

                    let generation_val = snapshot.generation;
                    let items_len = snapshot.items.len();

                    if did_clear {
                        // BRIDGE_RESET race: yield until acp_bridge writes new data
                        for wait_round in 0..15 {
                            tokio::task::yield_now().await;
                            let fresh = VIEW_MODELS.state().read().clone();
                            if fresh.generation != generation_val || !fresh.items.is_empty() {
                                info!(wait_round, generation_val = fresh.generation, len = fresh.items.len(), "render_bridge: BRIDGE_RESET race resolved");
                                break;
                            }
                        }
                    }

                    if generation_val == last_generation {
                        debug!(generation_val, last_generation, "render_bridge: NO_CHANGE, skipping rebuild");
                        continue;
                    }

                    // hash diff 优化
                    let new_hashes = extract_hashes_from_im(&snapshot.items);
                    let new_len = items_len;
                    let old_len = msg_hashes.len();
                    let stable = prefix_stable_len(&new_hashes, &msg_hashes);

                    debug!(
                        stable,
                        new_len,
                        old_len,
                        generation_val,
                        last_generation,
                        cache_entries = cache.entries.len(),
                        "render_bridge: generation changed, choosing rebuild strategy"
                    );

                    if new_len < old_len {
                        debug!("render_bridge: strategy=CLEAR_AND_REBUILD (len decreased)");
                        msg_hashes.clear();
                        cache.entries.clear();
                        rebuild_entries(&mut cache.entries, &snapshot.items, last_resize_width).await;
                    } else if stable > 0 && new_len >= old_len {
                        debug!(stable, "render_bridge: strategy=HASH_DIFF_APPEND");
                        while cache.entries.len() > stable {
                            cache.entries.pop();
                        }
                        let tail: Vec<crate::kit::tui_render_unit::TuiRenderUnit> =
                            snapshot.items.iter().skip(stable).cloned().collect();
                        append_entries(
                            &mut cache.entries,
                            &tail,
                            last_resize_width,
                            stable,
                        ).await;
                    } else if new_len > old_len && cache.entries.len() >= old_len {
                        debug!(
                            new_len,
                            old_len,
                            cache_entries = cache.entries.len(),
                            "render_bridge: strategy=INCREMENTAL_APPEND (len grew)"
                        );
                        let tail: Vec<crate::kit::tui_render_unit::TuiRenderUnit> =
                            snapshot.items.iter().skip(old_len).cloned().collect();
                        append_entries(
                            &mut cache.entries,
                            &tail,
                            last_resize_width,
                            old_len,
                        ).await;
                    } else {
                        debug!(
                            new_len,
                            old_len,
                            "render_bridge: strategy=FULL_REBUILD (fallback)"
                        );
                        rebuild_entries(&mut cache.entries, &snapshot.items, last_resize_width).await;
                    }

                    msg_hashes = new_hashes;

                    rebuild_cumulative_heights(&mut cache);
                    debug!(
                        cache_entries = cache.entries.len(),
                        "render_bridge: writing RENDER_CACHE"
                    );
                    *RENDER_CACHE.state().write() = cache.clone();
                    last_generation = generation_val;
                }
                else => break,
            }
        }
        debug!("render bridge exited");
    })
}

async fn read_ready_snapshot(
    last_generation: u64,
) -> Option<crate::kit::atoms::ViewModelsSnapshot> {
    // L7: yield 上限从 50 提到 200，前 50 轮纯 yield，50-200 轮之间每 10 轮插入一次
    // 1ms sleep（避免空转）。最坏情况 ~15ms 额外延迟，不影响吞吐。
    for round in 0..200 {
        let snapshot = VIEW_MODELS.state().read().clone();
        if snapshot.generation != last_generation {
            return Some(snapshot);
        }
        if round >= 50 && round % 10 == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    tracing::warn!(
        target = "render_bridge",
        "snapshot not ready after 200 yields"
    );
    None
}

fn extract_hashes_from_im(
    items: &im::Vector<crate::kit::tui_render_unit::TuiRenderUnit>,
) -> Vec<u64> {
    use crate::kit::tui_render_unit::TuiRenderUnit;
    items
        .iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiUserBubble(d) => d.content_hash,
            TuiRenderUnit::TuiAssistantBubble(d) => d.content_hash,
            TuiRenderUnit::TuiToolCard(d) => d.content_hash,
            TuiRenderUnit::TuiSubAgentGroup(d) => d.content_hash,
            TuiRenderUnit::TuiCollapsedGroup(d) => d.content_hash,
            TuiRenderUnit::TuiSystemNote(d) => d.content_hash,
            TuiRenderUnit::TuiDivider(d) => d.content_hash,
            TuiRenderUnit::TuiAskUserBlock(d) => d.content_hash,
        })
        .collect()
}

fn prefix_stable_len(new_hashes: &[u64], old_hashes: &[u64]) -> usize {
    new_hashes
        .iter()
        .zip(old_hashes.iter())
        .position(|(new_h, old_h)| new_h != old_h)
        .unwrap_or_else(|| old_hashes.len().min(new_hashes.len()))
}

/// L8: 返回内部读取的 snapshot，避免调用方在 rebuild_all 返回后再次读取
/// VIEW_MODELS atom 造成 snapshot 不一致（generation 可能已推进）。
async fn rebuild_all(
    width: usize,
    cache: &mut RenderCache,
    last_generation: &mut u64,
) -> crate::kit::atoms::ViewModelsSnapshot {
    let snapshot = VIEW_MODELS.state().read().clone();
    rebuild_entries(&mut cache.entries, &snapshot.items, width).await;
    rebuild_cumulative_heights(cache);
    *RENDER_CACHE.state().write() = cache.clone();
    *last_generation = snapshot.generation;
    snapshot
}

fn rebuild_entries(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    items: &im::Vector<crate::kit::tui_render_unit::TuiRenderUnit>,
    width: usize,
) -> impl std::future::Future<Output = ()> {
    entries.clear();
    let items_vec: Vec<crate::kit::tui_render_unit::TuiRenderUnit> =
        items.iter().cloned().collect();
    async move {
        append_entries(entries, &items_vec, width, 0).await;
    }
}

async fn append_entries(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    items: &[crate::kit::tui_render_unit::TuiRenderUnit],
    width: usize,
    start_index: usize,
) {
    const YIELD_EVERY: usize = 20;
    let mut next_yield_at: usize = YIELD_EVERY;
    for (offset, vm) in items.iter().enumerate() {
        let key = VmKey::Item(start_index + offset);
        for seg in view_render::render_v2_vm(vm, width) {
            match seg {
                MarkdownSegment::Text(lines) => {
                    let height = visual_height(&lines, width);
                    entries.push((
                        key.clone(),
                        RenderedEntry::Text {
                            height,
                            lines: Arc::from(lines),
                        },
                    ));
                }
                MarkdownSegment::Table(table) => {
                    let height = table_height(&table);
                    entries.push((
                        key.clone(),
                        RenderedEntry::Table {
                            height,
                            data: Arc::new(table),
                        },
                    ));
                }
            }
        }
        let call_count = view_render::RENDER_CALL_COUNT.with(|c| c.load(Ordering::Relaxed));
        if call_count >= next_yield_at {
            tokio::task::yield_now().await;
            next_yield_at =
                view_render::RENDER_CALL_COUNT.with(|c| c.load(Ordering::Relaxed)) + YIELD_EVERY;
        }
    }
    view_render::RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

/// 估算表格渲染高度（边框 + 表头 + 数据行）。
fn table_height(table: &crate::kit::markdown::TableData) -> usize {
    let header = usize::from(!table.headers.is_empty());
    3 + header + table.rows.len()
}

fn rebuild_cumulative_heights(cache: &mut RenderCache) {
    cache.cumulative_heights.clear();
    let mut sum = 0usize;
    for (_, entry) in &cache.entries {
        sum = sum.saturating_add(entry.height());
        cache.cumulative_heights.push(sum);
    }
}

pub fn visual_height(lines: &[Line<'static>], width: usize) -> usize {
    let w = width.max(1);
    let rows = lines.iter().fold(0usize, |s, l| {
        s.saturating_add(l.width().max(1).div_ceil(w))
    });
    rows.max(1)
}

pub fn build_wrap_map(lines: &[Line<'static>], width: u16) -> Vec<WrappedLineInfo> {
    let mut map = Vec::with_capacity(lines.len());
    // M2: 内部累加用 u32 避免大消息列表（>65535 视觉行）溢出。
    // WrappedLineInfo.visual_row 字段仍为 u16——push 时 saturate cast。
    let mut row: u32 = 0;
    let mut saturated_warned = false;
    for (line_idx, line) in lines.iter().enumerate() {
        let text = Text::from(line.clone());
        let height = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .line_count(width) as u16;
        let visual_height = height.max(1);
        if row > u16::MAX as u32 && !saturated_warned {
            tracing::warn!(
                target = "render_bridge",
                line_idx,
                row,
                "build_wrap_map: visual_row saturated at u16::MAX, viewport clip may be inaccurate"
            );
            saturated_warned = true;
        }
        let visual_row = row.min(u16::MAX as u32) as u16;
        map.push(WrappedLineInfo {
            line_idx,
            visual_row,
            visual_height,
        });
        row = row.saturating_add(visual_height as u32);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M2: 构造 70000 个单行 Line 喂 build_wrap_map，断言返回 Vec 长度正确，
    /// 且末尾若干项 visual_row 停在 u16::MAX 不再错误增长（字段类型仍为 u16）。
    #[test]
    fn test_build_wrap_map_u16_saturation_keeps_field_type() {
        let lines: Vec<Line<'static>> = (0..70_000).map(|_| Line::from("x")).collect();
        let map = build_wrap_map(&lines, 80);
        assert_eq!(map.len(), 70_000);
        let last = map.last().expect("non-empty");
        assert_eq!(last.visual_row, u16::MAX);
        let _: u16 = last.visual_row;
        let _: u16 = last.visual_height;
    }

    /// L2: RenderCache::default() 不应含 wrap_map 字段——
    /// 构造一个仅含 entries + cumulative_heights 的 RenderCache，断言可编译。
    #[test]
    fn test_render_cache_default_has_no_wrap_map_field() {
        let cache = RenderCache {
            entries: vec![],
            cumulative_heights: vec![],
        };
        assert_eq!(cache.entries.len(), 0);
        assert_eq!(cache.cumulative_heights.len(), 0);
        let _default: RenderCache = RenderCache::default();
    }
}
