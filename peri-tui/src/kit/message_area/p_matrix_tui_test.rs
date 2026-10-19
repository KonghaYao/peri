//! P03 / P04 / P05 原生测量：窗口翻页预算、背压与折叠/长工具输出回收。
//!
//! 所有用例使用真实 `BridgeState`、真实窗口接纳/淘汰/统一释放事务，以及真实
//! `Transcript::prepare` 渲染缓存路径；不修改生产语义。数字口径：
//!
//! - 历史正文：`HistoryWindow` 账本（页 JSON 编码字节）与 `committed` 对象字节；
//! - 渲染缓存：`Transcript::retained_bytes()`（真实 Slot/Markdown/行缓存）与
//!   `debug_cache_lengths()`（结构性投影缓存）；
//! - 在途页：窗口 `page_reserved_bytes` / `pending_merge_bytes` 与 consumer
//!   `WindowPagingState.reserved_bytes`；
//! - 不把任何单项数字冒充 RSS，也不承诺固定降幅。
//!
//! 运行方式（`--ignored`）：见 `.peri/plans/p-matrix.md`。
use super::grid::GridSpec;
use super::transcript::Transcript;
use super::vm_cache::VmCacheSlot;
use crate::kit::acp_events::{
    BridgeState, acp_events_test::make_fold_test_state, dispatch_for_bridge, push_view_models,
};
use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms::{
    self, FOLD_OVERRIDES, HISTORY_WINDOW_CAPABLE, HISTORY_WINDOW_PAGING, HISTORY_WINDOW_V1,
};
use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
use crate::kit::history_page_consumer::{
    self, WindowConsumerRequest, WindowPagePlacement, WindowRequestContext, claim_window_request,
    settle_window_response,
};
use crate::kit::history_paging::{
    WindowPageCursor, WindowPageItem, WindowPageResponse, WindowSubItem,
};
use crate::kit::history_window::{
    HistorySource, HistoryWindow, HistoryWindowConfig, WindowIdentity, WindowLimits,
};
use peri_theme::atoms::THEME_ATOM;
use serde_json::json;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;

#[path = "p_matrix_tui_test/p05_fold_and_tool_outputs.rs"]
mod p05_fold_and_tool_outputs;

const SESSION: &str = "s1";
const SNAPSHOT: &str = "snap-1";
const TICKET: &str = "ticket-1";
// ─── 夹具与探针 ──────────────────────────────────────────────────────────────
struct AtomsGuard;
impl Drop for AtomsGuard {
    fn drop(&mut self) {
        HISTORY_WINDOW_V1.set(false);
        HISTORY_WINDOW_CAPABLE.set(false);
        history_page_consumer::reset_window_paging_state();
        crate::kit::history_window::clear_window_viewport();
        crate::kit::history_window::clear_window_unit_mappings();
        crate::kit::acp_events::history_window::clear_pending_tool_ends();
        crate::kit::acp_events::render::invalidate_structural_caches();
        *FOLD_OVERRIDES.state().write() = HashMap::new();
        crate::kit::atoms::ACTIVE_SESSION_ID.state().write().clear();
    }
}

fn identity() -> WindowIdentity {
    WindowIdentity::new(SESSION, "gen-1", 7, SNAPSHOT)
}

fn limits(body_bytes: usize, reserved_bytes: usize, page_target: usize) -> WindowLimits {
    WindowLimits {
        body_bytes,
        reserved_bytes,
        page_target_bytes: page_target,
        ..Default::default()
    }
}

fn setup_window(window_limits: WindowLimits) -> (BridgeState, AtomsGuard) {
    atoms::init_atoms();
    HISTORY_WINDOW_V1.set(true);
    HISTORY_WINDOW_CAPABLE.set(true);
    crate::kit::acp_events::render::invalidate_structural_caches();
    history_page_consumer::reset_window_paging_state();
    *atoms::ACTIVE_SESSION_ID.state().write() = SESSION.to_string();

    let mut state = make_fold_test_state();
    state.active_session_id = SESSION.to_string();
    state.window = HistoryWindow::new(HistoryWindowConfig {
        history_window_v1: true,
        limits: window_limits,
    });
    state.window.begin_window(identity());
    history_page_consumer::note_window_generation(state.window.generation());
    {
        let handle = HISTORY_WINDOW_PAGING.state();
        let mut paging = handle.write();
        paging.session_id = SESSION.to_string();
        paging.identity = Some(identity());
        paging.window_generation = state.window.generation();
        paging.read_ticket = Some(TICKET.to_string());
    }
    (state, AtomsGuard)
}

fn text_item(entry_id: &str, position: u64, text: &str) -> WindowPageItem {
    WindowPageItem {
        source: HistorySource::Own,
        entry_id: format!("{SESSION}:{entry_id}"),
        position: Some(position),
        oversize: false,
        sub_items: vec![WindowSubItem {
            position: 0,
            update: json!({"sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text }, "messageId": entry_id}),
            system_reminder: false,
        }],
        descriptor: None,
        descriptor_only: false,
    }
}

fn response(items: Vec<WindowPageItem>) -> WindowPageResponse {
    WindowPageResponse {
        session_id: SESSION.to_string(),
        identity: identity(),
        committed_tail: None,
        checkpoint_id: None,
        read_ticket: TICKET.to_string(),
        before_cursor: Some("before".into()),
        after_cursor: Some("after".into()),
        items,
        body_descriptors: Vec::new(),
        descriptor_only_items: 0,
        inherited: None,
        inherited_unsupported: false,
        reopen_required: false,
    }
}

fn page_context(
    request_id: &str,
    reserved_bytes: usize,
    placement: WindowPagePlacement,
    window_generation: u64,
) -> WindowRequestContext {
    WindowRequestContext {
        request_id: request_id.to_string(),
        session_id: SESSION.to_string(),
        identity: identity(),
        window_generation,
        reserved_bytes,
        placement,
        is_open: false,
        display_generation: 0,
        gap: None,
    }
}

fn dispatch_page(
    state: &mut BridgeState,
    context: &WindowRequestContext,
    page: WindowPageResponse,
) {
    dispatch_for_bridge(
        state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context.clone()),
            response: Box::new(page),
        },
    );
}

fn render_context() -> EntryRenderContext {
    atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

/// 真实渲染缓存探针：持续复用同一 `Transcript` 与 `VmCacheSlot`。
struct RenderProbe {
    transcript: Transcript,
    caches: Vec<VmCacheSlot>,
    context: EntryRenderContext,
    grid: GridSpec,
    frame: u64,
}

impl RenderProbe {
    fn new() -> Self {
        let grid = GridSpec::grid_for(100);
        Self {
            transcript: Transcript::default(),
            caches: Vec::new(),
            context: render_context(),
            grid,
            frame: 0,
        }
    }

    /// 用当前 `VIEW_MODELS` 发布快照走一次真实 prepare，返回
    /// `(retained_bytes, cache_slots, published_units)`。
    fn measure(&mut self) -> (usize, usize, usize) {
        let snapshot = atoms::VIEW_MODELS.state().read().clone();
        self.frame = self.frame.saturating_add(1);
        let _index = self.transcript.prepare(
            &snapshot,
            &atoms::TranscriptPublication::default(),
            &mut self.caches,
            self.grid,
            self.grid.line_width(),
            self.context.clone(),
            self.frame,
            0,
            0,
            40,
            0,
        );
        (
            self.transcript.retained_bytes(),
            self.caches.len(),
            snapshot.items.len(),
        )
    }
}

fn fmib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn fmt_bytes(value: Option<u64>) -> String {
    value
        .map(|bytes| format!("{:.1}MiB", bytes as f64 / (1024.0 * 1024.0)))
        .unwrap_or_else(|| "n/a".into())
}

/// 进程级辅助观察：RSS + 项目现有 allocator 计数器。
/// macOS 系统分配器没有 allocated 计数，`query_stats().current_allocated` 镜像 RSS；
/// 因此这里只把 RSS 当作辅助，不用于证明正文泄漏（真实 allocator 见 P07）。
fn print_process(stage: &str) {
    let stats = crate::alloc_config::query_stats();
    println!(
        "  {stage} rss={} allocated_counter={} ({})",
        fmt_bytes(stats.map(|stats| stats.current_rss as u64)),
        fmt_bytes(stats.map(|stats| stats.current_allocated as u64)),
        if cfg!(target_os = "macos") {
            "macOS: counter mirrors RSS"
        } else {
            "allocator reported"
        }
    );
}

fn assert_budget(label: &str, observed: usize, limit: usize) {
    assert!(
        observed <= limit,
        "{label} 超预算：observed={observed}B limit={limit}B"
    );
}

// ─── P03：100 次双向翻页 ─────────────────────────────────────────────────────

const P03_PAGES: usize = 100;
const P03_ITEMS_PER_PAGE: usize = 4;
const P03_TEXT_BYTES: usize = 12 * 1024;
const P03_BODY_LIMIT: usize = 512 * 1024;
const P03_RESERVED_LIMIT: usize = 256 * 1024;
const P03_PAGE_TARGET: usize = 64 * 1024;

/// P03：100 次双向翻页（先向更早 50 页，再向更新 50 页并回到尾部）。
///
/// 记录历史正文、真实渲染缓存与在途页的峰值/稳态；判据是三者分别受配置预算
/// 约束，且不随已浏览总量（约 4.7 MiB 正文）线性增长。
#[test]
#[ignore = "P 矩阵测量（100 次双向窗口翻页）"]
#[serial]
fn p03_hundred_bidirectional_pages_stay_within_budgets() {
    let (mut state, _guard) =
        setup_window(limits(P03_BODY_LIMIT, P03_RESERVED_LIMIT, P03_PAGE_TARGET));
    let mut render = RenderProbe::new();
    let (render_budget, _render_target) = Transcript::render_cache_budgets();

    let mut total_browsed_bytes = 0usize;
    let mut max_resident_body = 0usize;
    let mut max_committed_body = 0usize;
    let mut max_page_reserved = 0usize;
    let mut max_pending_merge = 0usize;
    let mut max_render_bytes = 0usize;
    let mut max_slots = 0usize;
    let mut max_slot_delta = 0usize;
    let mut rows = Vec::new();

    println!("== P03 环境 ==");
    print_process("process(start)");
    println!(
        "  pages={P03_PAGES} items/page={P03_ITEMS_PER_PAGE} text={P03_TEXT_BYTES}B \
         body_limit={P03_BODY_LIMIT}B reserved_limit={P03_RESERVED_LIMIT}B \
         page_target={P03_PAGE_TARGET}B render_budget={render_budget}B"
    );

    for page_index in 0..P03_PAGES {
        let older = page_index < P03_PAGES / 2;
        let placement = if older {
            WindowPagePlacement::BeforeFirst
        } else {
            WindowPagePlacement::Tail
        };
        let base = if older {
            10_000u64.saturating_sub(page_index as u64 * 10)
        } else {
            20_000u64 + (page_index as u64 - 50) * 10
        };
        let items: Vec<WindowPageItem> = (0..P03_ITEMS_PER_PAGE)
            .map(|slot| {
                let position = if older {
                    base + slot as u64
                } else {
                    base + slot as u64
                };
                text_item(
                    &format!("p{page_index:03}-{slot}"),
                    position,
                    &format!("PAGE-{page_index:03}-{slot}-{}", "x".repeat(P03_TEXT_BYTES)),
                )
            })
            .collect();
        let page_body: usize = items
            .iter()
            .map(|item| {
                item.sub_items
                    .iter()
                    .map(|sub| {
                        serde_json::to_vec(&sub.update)
                            .map(|v| v.len())
                            .unwrap_or(0)
                    })
                    .sum::<usize>()
            })
            .sum();
        total_browsed_bytes += page_body;

        let context = page_context(
            &format!("req-{page_index:03}"),
            P03_PAGE_TARGET,
            placement,
            state.window.generation(),
        );
        dispatch_page(&mut state, &context, response(items));

        // 接纳已发生、发布尚未结算：这是本轮的在途/待合并峰值。
        let page_reserved = state.window.budget().page_reserved_bytes;
        let pending_merge = state.window.pending_merge_bytes();
        push_view_models(&mut state);
        let after_publication_reserved = state.window.budget().page_reserved_bytes;
        assert_eq!(
            after_publication_reserved, 0,
            "page {page_index}: 发布后必须归还页预留"
        );

        let bridge = state.memory_snapshot();
        let (render_bytes, slots, published_units) = render.measure();
        let budget = state.window.budget();

        assert_budget("resident_body", budget.resident_body_bytes, P03_BODY_LIMIT);
        assert_budget("page_reserved", page_reserved, P03_RESERVED_LIMIT);
        assert_budget("pending_merge", pending_merge, page_body.max(1));
        assert_budget("render_cache", render_bytes, render_budget);
        let slot_delta = state.committed.len() as isize - state.window.unit_count() as isize;
        assert!(
            (0..=P03_ITEMS_PER_PAGE as isize).contains(&slot_delta),
            "page {page_index}: committed 与窗口账本偏移必须不超过一页（observed {slot_delta}）"
        );
        max_slot_delta = max_slot_delta.max(slot_delta as usize);
        assert_eq!(slots, published_units, "渲染槽位必须与发布快照一致");

        max_resident_body = max_resident_body.max(budget.resident_body_bytes);
        max_committed_body = max_committed_body.max(bridge.committed_body_bytes);
        max_page_reserved = max_page_reserved.max(page_reserved);
        max_pending_merge = max_pending_merge.max(pending_merge);
        max_render_bytes = max_render_bytes.max(render_bytes);
        max_slots = max_slots.max(slots);

        rows.push((
            page_index + 1,
            budget.resident_body_bytes,
            bridge.committed_body_bytes,
            render_bytes,
            budget.page_reserved_bytes,
            budget.pending_merge_bytes,
            state.window.segment_count(),
            state.window.gap_count(),
        ));
    }

    println!("== P03 采样（每 10 页；峰值另列） ==");
    for row in rows.iter().filter(|row| row.0 % 10 == 0) {
        println!(
            "  page={:03} resident={}B committed_body={}B render_cache={}B \
             page_reserved={}B pending_merge={}B segments={} gaps={}",
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7
        );
    }
    let stats = state.window.stats();
    let diagnostics = state.window.diagnostics();
    println!(
        "P03 汇总：browsed={:.2}MiB max_resident={}B max_committed_body={}B \
         max_render={}B({:.2}MiB) max_slots={} max_page_reserved={}B \
         max_pending_merge={}B evicted_segments={} evicted_bytes={} gaps={} \
         admission_blocked={} stale_pages={} max_slot_delta={}",
        fmib(total_browsed_bytes),
        max_resident_body,
        max_committed_body,
        max_render_bytes,
        fmib(max_render_bytes),
        max_slots,
        max_page_reserved,
        max_pending_merge,
        stats.evicted_segments,
        stats.evicted_bytes,
        diagnostics.gap_count,
        stats.admission_blocked,
        stats.stale_pages,
        max_slot_delta,
    );

    assert!(
        total_browsed_bytes > P03_BODY_LIMIT * 5,
        "夹具必须真实超过窗口预算：browsed={total_browsed_bytes}B"
    );
    assert!(stats.evicted_segments > 0, "翻页必须真实触发淘汰");
    assert_budget("max_resident_body", max_resident_body, P03_BODY_LIMIT);
    assert_budget("max_page_reserved", max_page_reserved, P03_RESERVED_LIMIT);
    assert_budget("max_render_bytes", max_render_bytes, render_budget);
    // committed 的展示正文可以比窗口账本多至多一页（保守释放/展示偏移），
    // 但不得随浏览总量增长。
    assert!(
        max_committed_body <= P03_BODY_LIMIT + 64 * 1024,
        "committed 展示正文超预算：{}B",
        max_committed_body
    );
    // 稳态：最后 10 页不再随已浏览总量增长。
    let tail_max_resident = rows
        .iter()
        .rev()
        .take(10)
        .map(|row| row.1)
        .max()
        .unwrap_or(0);
    let tail_max_render = rows
        .iter()
        .rev()
        .take(10)
        .map(|row| row.3)
        .max()
        .unwrap_or(0);
    assert_budget("tail_resident_body", tail_max_resident, P03_BODY_LIMIT);
    assert_budget("tail_render_cache", tail_max_render, render_budget);
    println!("P03 稳态：tail10 max_resident={tail_max_resident}B max_render={tail_max_render}B");
    print_process("process(end)");
}

// ─── P04：慢消费者 + 快存储返回值（背压） ────────────────────────────────────

const P04_SUBMISSIONS: usize = 1000;
const P04_APPLY_EVERY: usize = 100;
const P04_PAGE_BODY: usize = 1024 * 1024;

/// P04：1000 次“存储已返回”的大页投递，消费者只每 100 次应用一次。
///
/// 真实 consumer 状态机（claim/settle/finish）限制单在途 + 单待合并；真正的
/// 桥接接纳/释放使用 `BridgeState`。记录未应用页数量/字节峰值与各拒绝原因，
/// 证明未应用结果不会积压成第二份全历史。
#[test]
#[ignore = "P 矩阵测量（慢消费者/快存储背压）"]
#[serial]
fn p04_slow_consumer_does_not_accumulate_unapplied_pages() {
    let (mut state, _guard) = setup_window(limits(4 * 1024 * 1024, 1024 * 1024, 512 * 1024));
    let mut held: Option<(WindowRequestContext, WindowPageResponse, usize)> = None;
    let mut max_unapplied_pages = 0usize;
    let mut max_unapplied_bytes = 0usize;
    let mut max_consumer_reserved = 0usize;
    let mut max_window_pending_merge = 0usize;
    let mut max_window_resident = 0usize;
    let mut granted = 0usize;
    let mut applied = 0usize;
    let mut rejected_in_flight = 0usize;
    let mut rejected_pending = 0usize;
    let mut discarded_bytes = 0usize;

    println!("== P04 环境 ==");
    print_process("process(start)");
    println!(
        "  submissions={P04_SUBMISSIONS} apply_every={P04_APPLY_EVERY} page_body={P04_PAGE_BODY}B \
         window_body_limit=4MiB window_pending_tool=1MiB"
    );

    for index in 0..P04_SUBMISSIONS {
        let request_id = format!("p04-{index:04}");
        let context = page_context(
            &request_id,
            512 * 1024,
            WindowPagePlacement::BeforeFirst,
            state.window.generation(),
        );
        let request = WindowConsumerRequest::Page {
            context: context.clone(),
            cursor: WindowPageCursor::Before(format!("cursor-{index:04}")),
            limit: 50,
        };
        match claim_window_request(&request) {
            history_page_consumer::WindowClaim::Granted(in_flight) => {
                granted += 1;
                // “存储返回”：构建一页 1 MiB 正文，但先不消费。
                let body = format!("P04-{index:04}-{}", "y".repeat(P04_PAGE_BODY));
                let page = response(vec![text_item(
                    &format!("p04-{index:04}"),
                    1_000_000 + index as u64,
                    &body,
                )]);
                let page_bytes = page
                    .items
                    .iter()
                    .flat_map(|item| item.sub_items.iter())
                    .map(|sub| {
                        serde_json::to_vec(&sub.update)
                            .map(|v| v.len())
                            .unwrap_or(0)
                    })
                    .sum::<usize>();
                match settle_window_response(&in_flight, &page) {
                    history_page_consumer::WindowSettlement::Accepted(_) => {
                        held = Some((context, page, page_bytes));
                        max_unapplied_pages = max_unapplied_pages.max(1);
                        max_unapplied_bytes = max_unapplied_bytes.max(page_bytes);
                    }
                    other => panic!("unexpected settlement: {other:?}"),
                }
            }
            history_page_consumer::WindowClaim::Rejected(rejection) => {
                match rejection {
                    history_page_consumer::WindowClaimRejection::InFlight => {
                        rejected_in_flight += 1
                    }
                    history_page_consumer::WindowClaimRejection::PendingMerge => {
                        rejected_pending += 1
                    }
                    other => panic!("unexpected claim rejection: {other:?}"),
                }
                // 存储“返回”了但消费者尚未取走：直接丢弃，不进入任何队列。
                let body_bytes = serde_json::to_vec(&json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "z".repeat(P04_PAGE_BODY) },
                }))
                .map(|value| value.len())
                .unwrap_or(0);
                discarded_bytes = discarded_bytes.saturating_add(body_bytes);
            }
            history_page_consumer::WindowClaim::Merged => {
                panic!("distinct cursors must not be merged")
            }
            history_page_consumer::WindowClaim::Disabled => panic!("window path disabled"),
        }

        let consumer = HISTORY_WINDOW_PAGING.state().read().clone();
        max_consumer_reserved = max_consumer_reserved.max(consumer.reserved_bytes);
        max_window_pending_merge =
            max_window_pending_merge.max(state.window.budget().pending_merge_bytes);
        max_window_resident = max_window_resident.max(state.window.budget().resident_body_bytes);

        // 慢消费：每 APPLY_EVERY 次存储返回才把待合并页交给桥接发布一次。
        let should_apply = index % P04_APPLY_EVERY == P04_APPLY_EVERY - 1;
        if should_apply && let Some((context, page, page_bytes)) = held.take() {
            dispatch_page(&mut state, &context, page);
            max_window_pending_merge =
                max_window_pending_merge.max(state.window.budget().pending_merge_bytes);
            push_view_models(&mut state);
            applied += 1;
            assert_eq!(
                state.window.budget().page_reserved_bytes,
                0,
                "发布后必须归还窗口页预留"
            );
            assert_eq!(
                HISTORY_WINDOW_PAGING.state().read().reserved_bytes,
                0,
                "发布后必须归还 consumer 页预留"
            );
            max_unapplied_pages = max_unapplied_pages.max(held.is_some() as usize);
            max_unapplied_bytes = max_unapplied_bytes.max(page_bytes);
        }
    }

    // 收尾：应用最后一个待合并页（若有）。
    if let Some((context, page, page_bytes)) = held.take() {
        dispatch_page(&mut state, &context, page);
        push_view_models(&mut state);
        applied += 1;
        max_unapplied_bytes = max_unapplied_bytes.max(page_bytes);
    }
    let stats = state.window.stats();
    let diagnostics = state.window.diagnostics();
    println!(
        "P04 汇总：submissions={P04_SUBMISSIONS} granted={granted} applied={applied} \
         rejected_in_flight={rejected_in_flight} rejected_pending={rejected_pending} \
         max_unapplied_pages={max_unapplied_pages} max_unapplied_bytes={}B \
         max_consumer_reserved={}B max_window_pending_merge={}B max_window_resident={}B \
         discarded_response_bytes={}B evicted_segments={} admission_blocked={}",
        max_unapplied_bytes,
        max_consumer_reserved,
        max_window_pending_merge,
        max_window_resident,
        discarded_bytes,
        stats.evicted_segments,
        diagnostics.admission_blocked,
    );

    assert!(granted > 0 && applied == granted, "每个获准页都必须被消费");
    assert_eq!(max_unapplied_pages, 1, "未应用页不得超过一个");
    assert!(
        max_unapplied_bytes <= P04_PAGE_BODY + 4096,
        "未应用字节不得超过单页正文"
    );
    assert!(
        max_consumer_reserved <= 512 * 1024,
        "consumer 预留不得超过单页预留"
    );
    assert!(
        max_unapplied_bytes * 2 < P04_SUBMISSIONS * P04_PAGE_BODY,
        "未应用页不得积压成第二份全历史"
    );
    assert_budget("max_window_resident", max_window_resident, 4 * 1024 * 1024);
    print_process("process(end)");
}
