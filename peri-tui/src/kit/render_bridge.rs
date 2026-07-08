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
use crate::kit::atoms::{RENDER_CACHE, VIEW_MODELS};
use crate::kit::view_render;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VmKey {
    Item(usize),
}

#[derive(Debug, Clone)]
pub struct RenderedEntry {
    pub height: usize,
    pub lines: Arc<[Line<'static>]>,
}

#[derive(Debug, Clone, Default)]
pub struct RenderCache {
    pub entries: Vec<(VmKey, RenderedEntry)>,
    pub cumulative_heights: Vec<usize>,
    /// 每逻辑行的视觉行映射——message_area 视口裁剪用
    pub wrap_map: Vec<WrappedLineInfo>,
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
        let mut msg_lines_cache: Vec<Vec<ratatui::text::Line<'static>>> = Vec::new();

        // 每秒轮询 VIEW_MODELS atom——检测 generation 变化（如 running Bash 计时器）
        let mut poll_interval = tokio::time::interval(std::time::Duration::from_secs(1));
        poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = poll_interval.tick() => {
                    let snapshot = VIEW_MODELS.state().read().clone();
                    if snapshot.generation != last_generation {
                        debug!(generation = snapshot.generation, last_generation, "render_bridge: tick FULL_REBUILD (generation changed)");
                        rebuild_entries(&mut cache.entries, &snapshot.items, last_resize_width).await;
                        msg_hashes = extract_hashes_from_im(&snapshot.items);
                        rebuild_cumulative_heights(&mut cache);
                        let all_lines: Vec<Line<'static>> = cache
                            .entries
                            .iter()
                            .flat_map(|(_, entry)| entry.lines.iter())
                            .cloned()
                            .collect();
                        cache.wrap_map = build_wrap_map(&all_lines, last_resize_width as u16);
                        *RENDER_CACHE.state().write() = cache.clone();
                        last_generation = snapshot.generation;
                    }
                }
                Some(new_width) = resize_rx.recv() => {
                    let new_width = usize::from(new_width).max(1);
                    if new_width != last_resize_width {
                        last_resize_width = new_width;
                        rebuild_all(last_resize_width, &mut cache, &mut last_generation).await;
                        let snapshot = VIEW_MODELS.state().read().clone();
                        msg_hashes = extract_hashes_from_im(&snapshot.items);
                        msg_lines_cache.clear();
                    }
                }
                Some(_epoch_event) = rx.recv() => {
                    // BRIDGE_RESET_COUNTER 变更 → 清空缓存
                    let counter = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
                    let did_clear = if counter != last_reset_counter {
                        last_reset_counter = counter;
                        cache = RenderCache::default();
                        msg_hashes.clear();
                        msg_lines_cache.clear();
                        last_generation = 0;
                        *RENDER_CACHE.state().write() = cache.clone();
                        info!(counter, "render_bridge: cache cleared by BRIDGE_RESET_COUNTER");
                        true
                    } else {
                        false
                    };

                    let Some(snapshot) = read_ready_snapshot(last_generation).await else {
                        info!("render_bridge: event dropped (generation unchanged after 50 yields)");
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
                        msg_lines_cache.clear();
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
                    let all_lines: Vec<Line<'static>> = cache
                        .entries
                        .iter()
                        .flat_map(|(_, entry)| entry.lines.iter())
                        .cloned()
                        .collect();
                    cache.wrap_map = build_wrap_map(&all_lines, last_resize_width as u16);
                    debug!(
                        cache_entries = cache.entries.len(),
                        wrap_map_len = cache.wrap_map.len(),
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
    for _ in 0..50 {
        let snapshot = VIEW_MODELS.state().read().clone();
        if snapshot.generation != last_generation {
            return Some(snapshot);
        }
        tokio::task::yield_now().await;
    }
    tracing::warn!(
        target = "render_bridge",
        "snapshot not ready after 50 yields"
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

async fn rebuild_all(width: usize, cache: &mut RenderCache, last_generation: &mut u64) {
    let snapshot = VIEW_MODELS.state().read().clone();
    rebuild_entries(&mut cache.entries, &snapshot.items, width).await;
    rebuild_cumulative_heights(cache);
    let all_lines: Vec<Line<'static>> = cache
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter())
        .cloned()
        .collect();
    cache.wrap_map = build_wrap_map(&all_lines, width as u16);
    *RENDER_CACHE.state().write() = cache.clone();
    *last_generation = snapshot.generation;
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
        let lines = view_render::render_v2_vm(vm, width);
        let height = visual_height(&lines, width);
        entries.push((
            key,
            RenderedEntry {
                height,
                lines: Arc::from(lines),
            },
        ));
        let call_count = view_render::RENDER_CALL_COUNT.with(|c| c.load(Ordering::Relaxed));
        if call_count >= next_yield_at {
            tokio::task::yield_now().await;
            next_yield_at =
                view_render::RENDER_CALL_COUNT.with(|c| c.load(Ordering::Relaxed)) + YIELD_EVERY;
        }
    }
    view_render::RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

fn rebuild_cumulative_heights(cache: &mut RenderCache) {
    cache.cumulative_heights.clear();
    let mut sum = 0usize;
    for (_, entry) in &cache.entries {
        sum = sum.saturating_add(entry.height);
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
    let mut row: u16 = 0;
    for (line_idx, line) in lines.iter().enumerate() {
        let text = Text::from(line.clone());
        let height = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .line_count(width) as u16;
        let visual_height = height.max(1);
        map.push(WrappedLineInfo {
            line_idx,
            visual_row: row,
            visual_height,
        });
        row = row.saturating_add(visual_height);
    }
    map
}
