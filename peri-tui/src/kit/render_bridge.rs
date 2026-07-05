//! MessageArea 渲染缓存预处理桥。
//!
//! 独立 tokio task 监听 ACP 流式事件与宽度变化，预计算每条 ViewModel 的
//! `Vec<Line<'static>>` 与可视高度，并写入 `RENDER_CACHE` atom。

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms::{RENDER_CACHE, VIEW_MODELS};
use crate::kit::view_render;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VmKey {
    Committed(usize),
    CurrentTurn(usize),
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
    /// 在 rebuild + dedup 后基于所有行重新计算
    pub wrap_map: Vec<WrappedLineInfo>,
}

/// 每条逻辑行的 wrap 信息——用于视口裁剪的二分查找
#[derive(Debug, Clone)]
pub struct WrappedLineInfo {
    /// 在 render_cache 的全量 lines（所有 entries 展开）中的索引
    pub line_idx: usize,
    /// 该逻辑行渲染后的起始视觉行号（从 0 开始）
    pub visual_row: u16,
    /// 该逻辑行的视觉行数（>= 1，考虑 wrap）
    pub visual_height: u16,
}

pub fn spawn_render_bridge(
    mut rx: UnboundedReceiver<AcpEventData>,
    mut resize_rx: UnboundedReceiver<u16>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut width: usize = 80;
        let mut last_committed_ptr: usize = 0;
        let mut last_committed_len: usize = 0;
        let mut last_ct_ptr: usize = 0;
        let mut cache = RenderCache::default();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(new_width) = resize_rx.recv() => {
                    let new_width = usize::from(new_width).max(1);
                    if new_width != width {
                        width = new_width;
                        rebuild_all(width, &mut cache, &mut last_committed_ptr, &mut last_committed_len, &mut last_ct_ptr).await;
                    }
                }
                Some(_event) = rx.recv() => {
                    let Some(snapshot) = read_ready_snapshot(last_committed_ptr, last_committed_len, last_ct_ptr).await else {
                        info!("render_bridge: event dropped (VIEW_MODELS unchanged after 5 retries)");
                        continue;
                    };
                    log_ct_snapshot(&snapshot, "render_bridge: processing snapshot");
                    let committed_ptr = Arc::as_ptr(&snapshot.committed) as *const () as usize;
                    let committed_len = snapshot.committed.len();
                    let ct_ptr = Arc::as_ptr(&snapshot.current_turn) as *const () as usize;

                    if committed_ptr == last_committed_ptr
                        && committed_len == last_committed_len
                        && ct_ptr == last_ct_ptr
                    {
                        continue;
                    }

                    if committed_ptr != last_committed_ptr {
                        if committed_len > last_committed_len && cache.entries.len() >= last_committed_len {
                            cache.entries.retain(|(key, _)| !matches!(key, VmKey::CurrentTurn(_)));
                            append_entries(
                                &mut cache.entries,
                                &snapshot.committed[last_committed_len..],
                                width,
                                last_committed_len,
                                true,
                            ).await;
                            rebuild_current_turn(&mut cache.entries, &snapshot.current_turn, width).await;
                        } else {
                            rebuild_entries(&mut cache.entries, &snapshot.committed, &snapshot.current_turn, width).await;
                        }
                    } else if ct_ptr != last_ct_ptr {
                        rebuild_current_turn(&mut cache.entries, &snapshot.current_turn, width).await;
                    }

                    rebuild_cumulative_heights(&mut cache);
                    // 在所有 entry 追加/重建完成后，构建 wrap_map
                    let all_lines: Vec<Line<'static>> = cache
                        .entries
                        .iter()
                        .flat_map(|(_, entry)| entry.lines.iter())
                        .cloned()
                        .collect();
                    cache.wrap_map = build_wrap_map(&all_lines, width as u16);
                    *RENDER_CACHE.state().write() = cache.clone();
                    last_committed_ptr = committed_ptr;
                    last_committed_len = committed_len;
                    last_ct_ptr = ct_ptr;
                }
                else => break,
            }
        }
        debug!("render bridge exited");
    })
}

async fn read_ready_snapshot(
    last_committed_ptr: usize,
    last_committed_len: usize,
    last_ct_ptr: usize,
) -> Option<crate::kit::atoms::ViewModelsSnapshot> {
    for _ in 0..5 {
        let snapshot = VIEW_MODELS.state().read().clone();
        let committed_ptr = Arc::as_ptr(&snapshot.committed) as *const () as usize;
        let committed_len = snapshot.committed.len();
        let ct_ptr = Arc::as_ptr(&snapshot.current_turn) as *const () as usize;
        if committed_ptr != last_committed_ptr
            || committed_len != last_committed_len
            || ct_ptr != last_ct_ptr
        {
            return Some(snapshot);
        }
        tokio::task::yield_now().await;
    }
    None
}

fn log_ct_snapshot(snapshot: &crate::kit::atoms::ViewModelsSnapshot, label: &str) {
    info!(
        label,
        committed_len = snapshot.committed.len(),
        ct_len = snapshot.current_turn.len(),
    );
    for vm in snapshot.current_turn.iter() {
        if let peri_acp_types::view_model::ViewModel::AssistantBubble(data) = vm {
            info!(
                "  CT AssistantBubble text_len={} has_reasoning={}",
                data.text.len(),
                data.reasoning.is_some()
            );
        }
    }
}

async fn rebuild_all(
    width: usize,
    cache: &mut RenderCache,
    last_committed_ptr: &mut usize,
    last_committed_len: &mut usize,
    last_ct_ptr: &mut usize,
) {
    let snapshot = VIEW_MODELS.state().read().clone();
    rebuild_entries(
        &mut cache.entries,
        &snapshot.committed,
        &snapshot.current_turn,
        width,
    )
    .await;
    rebuild_cumulative_heights(cache);
    let all_lines: Vec<Line<'static>> = cache
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter())
        .cloned()
        .collect();
    cache.wrap_map = build_wrap_map(&all_lines, width as u16);
    *RENDER_CACHE.state().write() = cache.clone();
    *last_committed_ptr = Arc::as_ptr(&snapshot.committed) as *const () as usize;
    *last_committed_len = snapshot.committed.len();
    *last_ct_ptr = Arc::as_ptr(&snapshot.current_turn) as *const () as usize;
}

fn rebuild_entries(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    committed: &[peri_acp_types::view_model::ViewModel],
    current_turn: &[peri_acp_types::view_model::ViewModel],
    width: usize,
) -> impl std::future::Future<Output = ()> {
    entries.clear();
    async move {
        append_entries(entries, committed, width, 0, true).await;
        append_entries(entries, current_turn, width, 0, false).await;
    }
}

fn rebuild_current_turn(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    current_turn: &[peri_acp_types::view_model::ViewModel],
    width: usize,
) -> impl std::future::Future<Output = ()> {
    entries.retain(|(key, _)| !matches!(key, VmKey::CurrentTurn(_)));
    async move {
        append_entries(entries, current_turn, width, 0, false).await;
    }
}

async fn append_entries(
    entries: &mut Vec<(VmKey, RenderedEntry)>,
    items: &[peri_acp_types::view_model::ViewModel],
    width: usize,
    start_index: usize,
    committed: bool,
) {
    const YIELD_EVERY: usize = 20;
    let mut next_yield_at: usize = YIELD_EVERY;
    for (offset, vm) in items.iter().enumerate() {
        let key = if committed {
            VmKey::Committed(start_index + offset)
        } else {
            VmKey::CurrentTurn(offset)
        };
        let lines = view_render::render_v2_vm(vm, width, false);
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

/// 空行去重——连续空行只保留一个，移除末尾多余空行。
/// peri-main 的 rebuild() 在拼接所有消息行后做此操作，确保 wrap_map 行号准确。
pub fn dedup_lines(lines: &[Line<'static>]) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        let is_empty = line.spans.is_empty() || line.spans.iter().all(|s| s.content.is_empty());
        if is_empty
            && result.last().map_or(false, |l: &Line| {
                l.spans.is_empty() || l.spans.iter().all(|s| s.content.is_empty())
            })
        {
            continue;
        }
        result.push(line.clone());
    }
    while result.last().map_or(false, |l| l.spans.is_empty()) {
        result.pop();
    }
    result
}

/// 基于所有 lines 构建 wrap_map。
/// 使用 ratatui Paragraph::line_count() 确保与真实渲染的 WordWrapper 完全一致（含 CJK 支持）。
pub fn build_wrap_map(lines: &[Line<'static>], width: u16) -> Vec<WrappedLineInfo> {
    let deduped = dedup_lines(lines);
    let mut map = Vec::with_capacity(deduped.len());
    let mut row: u16 = 0;
    for (line_idx, line) in deduped.iter().enumerate() {
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
