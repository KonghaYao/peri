//! P05：频繁折叠 + 长工具输出 + 跨页工具配对（自 `p_matrix_tui_test` 拆分）。

use super::*;

use crate::kit::history_window::{
    HistoryEntryView, PersistentEntryKey, ReleasePlan, ReleaseReason, ResidentIndex, ResultRef,
    SegmentDraft, SegmentPlacement, ToolTerminalMeta, release_window_plans,
};
use crate::kit::tui_render_unit::{FoldKey, FoldState};

fn tool_start_item(entry_id: &str, position: u64, tool_id: &str) -> WindowPageItem {
    WindowPageItem {
        source: HistorySource::Own,
        entry_id: format!("{SESSION}:{entry_id}"),
        position: Some(position),
        oversize: false,
        sub_items: vec![WindowSubItem {
            position: 0,
            update: json!({"sessionUpdate": "tool_call", "toolCallId": tool_id, "title": "Bash", "rawInput": { "command": "true" }}),
            system_reminder: false,
        }],
        descriptor: None,
        descriptor_only: false,
    }
}

fn tool_end_item(entry_id: &str, position: u64, tool_id: &str, output: &str) -> WindowPageItem {
    WindowPageItem {
        source: HistorySource::Own,
        entry_id: format!("{SESSION}:{entry_id}"),
        position: Some(position),
        oversize: false,
        sub_items: vec![WindowSubItem {
            position: 0,
            update: json!({"sessionUpdate": "tool_call_update", "toolCallId": tool_id, "rawOutput": output, "status": "completed"}),
            system_reminder: false,
        }],
        descriptor: None,
        descriptor_only: false,
    }
}

// ─── P05：频繁折叠 + 长工具输出 + 跨页工具配对 ───────────────────────────────

const P05_PAGES: usize = 60;
const P05_TOOL_OUTPUT: usize = 256 * 1024;
const P05_BODY_LIMIT: usize = 2 * 1024 * 1024;
const P05_PENDING_TOOL_BYTES: usize = 1024 * 1024;
const P05_PENDING_TOOL_RESULTS: usize = 256;

/// P05：60 页长工具输出 + 每页折叠切换；跨页工具终点由轻量摘要承接。
///
/// 记录历史正文、渲染缓存、折叠投影与 PENDING_TOOL_ENDS 的峰值；淘汰/释放后
/// 未配对终点被清空。另用真实 `ResidentIndex` 验证跨页配对预算与段释放剪枝。
#[test]
#[ignore = "P 矩阵测量（频繁折叠/长工具输出/跨页配对）"]
#[serial]
fn p05_fold_and_long_tool_outputs_reclaim_caches_and_pairing() {
    let window_limits = WindowLimits {
        body_bytes: P05_BODY_LIMIT,
        reserved_bytes: 1024 * 1024,
        page_target_bytes: 256 * 1024,
        pending_tool_bytes: P05_PENDING_TOOL_BYTES,
        max_pending_tool_results: P05_PENDING_TOOL_RESULTS,
        ..Default::default()
    };
    let (mut state, _guard) = setup_window(window_limits);
    let mut render = RenderProbe::new();
    let (render_budget, _) = Transcript::render_cache_budgets();

    let mut total_tool_bytes = 0usize;
    let mut max_resident = 0usize;
    let mut max_committed = 0usize;
    let mut max_folded = 0usize;
    let mut max_render = 0usize;
    let mut max_pending_entries = 0usize;
    let mut max_pending_bytes = 0usize;
    let mut max_structural_caches = 0usize;
    let mut rows = Vec::new();

    println!("== P05 环境 ==");
    print_process("process(start)");
    println!(
        "  pages={P05_PAGES} tool_output={P05_TOOL_OUTPUT}B body_limit={P05_BODY_LIMIT}B \
         pending_tool_budget={P05_PENDING_TOOL_BYTES}B/{P05_PENDING_TOOL_RESULTS}条 \
         render_budget={render_budget}B"
    );

    for page_index in 0..P05_PAGES {
        let position_base = 30_000 + page_index as u64 * 10;
        let completed_tool = format!("tool-c-{page_index:03}");
        let pending_tool = format!("tool-p-{page_index:03}");
        let output = format!("TOOL-OUT-{page_index:03}-{}", "o".repeat(P05_TOOL_OUTPUT));
        total_tool_bytes += output.len() * 2;
        let items = vec![
            // 页内完整配对：长输出进入驻留卡片正文。
            tool_start_item(
                &format!("c{page_index:03}-s"),
                position_base,
                &completed_tool,
            ),
            tool_end_item(
                &format!("c{page_index:03}-e"),
                position_base + 1,
                &completed_tool,
                &output,
            ),
            // 跨页配对：终点先到（起点在另一页/未物化），只保留轻量摘要。
            tool_end_item(
                &format!("p{page_index:03}-e"),
                position_base + 2,
                &pending_tool,
                &output,
            ),
        ];
        let context = page_context(
            &format!("p05-req-{page_index:03}"),
            256 * 1024,
            WindowPagePlacement::Tail,
            state.window.generation(),
        );
        dispatch_page(&mut state, &context, response(items));
        push_view_models(&mut state);

        // 频繁折叠：在自动折叠与用户覆盖之间来回切换同一驻留工具卡。
        {
            let overrides_state = FOLD_OVERRIDES.state();
            let mut overrides = overrides_state.write();
            overrides.insert(
                FoldKey::Tool(completed_tool.clone()),
                if page_index % 2 == 0 {
                    FoldState::Expanded
                } else {
                    FoldState::Collapsed
                },
            );
        }
        push_view_models(&mut state);

        let bridge = state.memory_snapshot();
        let (render_bytes, _slots, _units) = render.measure();
        let (pending_entries, pending_bytes) =
            crate::kit::acp_events::history_window::pending_tool_ends_fingerprint();
        let structural = crate::kit::acp_events::render::debug_cache_lengths();
        let structural_max = [structural.0, structural.1, structural.2]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(0);

        assert_budget(
            "resident_body",
            state.window.budget().resident_body_bytes,
            P05_BODY_LIMIT,
        );
        assert_budget("render_cache", render_bytes, render_budget);
        assert_budget(
            "pending_tool_entries",
            pending_entries,
            P05_PENDING_TOOL_RESULTS,
        );
        assert_budget("pending_tool_bytes", pending_bytes, P05_PENDING_TOOL_BYTES);

        max_resident = max_resident.max(state.window.budget().resident_body_bytes);
        max_committed = max_committed.max(bridge.committed_body_bytes);
        max_folded = max_folded.max(bridge.folded_body_bytes);
        max_render = max_render.max(render_bytes);
        max_pending_entries = max_pending_entries.max(pending_entries);
        max_pending_bytes = max_pending_bytes.max(pending_bytes);
        max_structural_caches = max_structural_caches.max(structural_max);
        rows.push((
            page_index + 1,
            state.window.budget().resident_body_bytes,
            bridge.committed_body_bytes,
            bridge.folded_body_bytes,
            render_bytes,
            pending_entries,
            pending_bytes,
        ));
    }

    // 强制一次淘汰（纯文本大页，不新增未配对终点），验证释放事务清空配对暂存。
    // 同时把未配对终点压到固定上限（256 条 / 1 MiB，与桥接常量同源）。
    for slot in 0..400 {
        crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
            &format!("stress-tool-{slot:03}"),
            &"q".repeat(4096),
            false,
        );
    }
    let (stress_entries, stress_bytes) =
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint();
    assert_budget(
        "stress_pending_entries",
        stress_entries,
        P05_PENDING_TOOL_RESULTS,
    );
    assert_budget("stress_pending_bytes", stress_bytes, P05_PENDING_TOOL_BYTES);
    println!(
        "P05 未配对终点压力：entries={stress_entries} bytes={stress_bytes}B \
         (caps {P05_PENDING_TOOL_RESULTS} / {P05_PENDING_TOOL_BYTES}B)"
    );

    let text_items: Vec<WindowPageItem> = (0..2)
        .map(|slot| {
            text_item(
                &format!("final-{slot}"),
                90_000 + slot as u64,
                &format!("FINAL-{slot}-{}", "f".repeat(300 * 1024)),
            )
        })
        .collect();
    let context = page_context(
        "p05-final",
        512 * 1024,
        WindowPagePlacement::Tail,
        state.window.generation(),
    );
    dispatch_page(&mut state, &context, response(text_items));
    push_view_models(&mut state);
    // 无论最后一次接纳是否因“单页过大”被阻塞，都走同一条统一释放事务移除一个
    // 驻留段（真实业务路径，不是测试后门），验证配对暂存随释放归零。
    state.window.set_anchor(None);
    if let Some(segment_key) = state.window.segments().keys().next().copied() {
        let segment = state.window.segment(segment_key).expect("segment").clone();
        let plan = ReleasePlan {
            segment: segment_key,
            source: segment.source.clone(),
            entries: segment.entry_keys(),
            unit_count: segment.unit_count,
            body_bytes: segment.body_bytes,
            reserved_bytes: segment.reserved_bytes,
            slot_range: state.window.index().slot_range(segment_key),
            before: None,
            after: None,
            before_cursor: None,
            after_cursor: None,
        };
        let outcome = release_window_plans(&mut state, vec![plan], ReleaseReason::BudgetPressure);
        assert!(outcome.released_entries > 0, "统一释放事务必须释放驻留段");
        push_view_models(&mut state);
    }
    let (pending_after_release_entries, pending_after_release_bytes) =
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint();
    let resident_after_release = state.window.budget().resident_body_bytes;

    println!("== P05 采样（每 10 页；峰值另列） ==");
    for row in rows.iter().filter(|row| row.0 % 10 == 0) {
        println!(
            "  page={:03} resident={}B committed_body={}B folded_body={}B \
             render_cache={}B pending_tool={}条/{}B",
            row.0, row.1, row.2, row.3, row.4, row.5, row.6
        );
    }
    let stats = state.window.stats();
    println!(
        "P05 汇总：browsed_tool_body={:.2}MiB max_resident={}B max_committed={}B \
         max_folded={}B max_render={}B({:.2}MiB) max_structural_cache={} \
         max_pending_tool={}条/{}B after_release={}条/{}B resident_after_release={}B \
         evicted_segments={} evicted_bytes={}",
        fmib(total_tool_bytes),
        max_resident,
        max_committed,
        max_folded,
        max_render,
        fmib(max_render),
        max_structural_caches,
        max_pending_entries,
        max_pending_bytes,
        pending_after_release_entries,
        pending_after_release_bytes,
        resident_after_release,
        stats.evicted_segments,
        stats.evicted_bytes,
    );

    assert!(
        total_tool_bytes > P05_BODY_LIMIT * 5,
        "夹具必须真实超过窗口预算"
    );
    assert!(stats.evicted_segments > 0, "长工具输出浏览必须触发释放");
    assert_budget("max_resident", max_resident, P05_BODY_LIMIT);
    assert_budget("max_render", max_render, render_budget);
    assert_budget(
        "max_pending_tool_entries",
        max_pending_entries,
        P05_PENDING_TOOL_RESULTS,
    );
    assert_budget(
        "max_pending_tool_bytes",
        max_pending_bytes,
        P05_PENDING_TOOL_BYTES,
    );
    assert_eq!(
        (pending_after_release_entries, pending_after_release_bytes),
        (0, 0),
        "释放事务必须清空未配对工具终点"
    );
    assert_budget(
        "resident_after_release",
        resident_after_release,
        P05_BODY_LIMIT,
    );
    // 稳态：末尾 10 页的驻留正文不超过预算，渲染缓存不随浏览总量增长。
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
        .map(|row| row.4)
        .max()
        .unwrap_or(0);
    assert_budget("tail_resident", tail_max_resident, P05_BODY_LIMIT);
    assert_budget("tail_render", tail_max_render, render_budget);

    // ── 真实 ResidentIndex：跨页配对预算 + 段释放剪枝 ──────────────────────
    let mut index = ResidentIndex::new(P05_PENDING_TOOL_BYTES, P05_PENDING_TOOL_RESULTS);
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for slot in 0..(P05_PENDING_TOOL_RESULTS * 4) {
        let result = index.record_tool_result(
            HistorySource::Own,
            format!("pair-{slot:04}"),
            ResultRef::new(
                HistorySource::Own,
                format!("{SESSION}:result-{slot:04}"),
                "rev",
            ),
            ToolTerminalMeta::new(false, 64 * 1024, "s".repeat(1024)),
        );
        match result {
            Ok(_) => accepted += 1,
            Err(_) => rejected += 1,
        }
    }
    let pairing_bytes = index.pending_tool_bytes();
    println!(
        "P05 ResidentIndex：result_only accepted={accepted} rejected={rejected} \
         pending_bytes={pairing_bytes}B (limit {P05_PENDING_TOOL_BYTES}B)"
    );
    assert!(rejected > 0, "超出预算的跨页配对必须被拒绝");
    assert_budget(
        "resident_index_pending",
        pairing_bytes,
        P05_PENDING_TOOL_BYTES,
    );

    // 段释放后，指向被释放条目的 ResultOnly 配对必须一起消失。
    let mut window = HistoryWindow::new(HistoryWindowConfig {
        history_window_v1: true,
        limits: WindowLimits {
            pending_tool_bytes: P05_PENDING_TOOL_BYTES,
            max_pending_tool_results: P05_PENDING_TOOL_RESULTS,
            ..Default::default()
        },
    });
    window.begin_window(identity());
    let entries: Vec<HistoryEntryView> = (0..8)
        .map(|slot| {
            HistoryEntryView::new(
                PersistentEntryKey::own(format!("{SESSION}:pair-{slot:04}")),
                Some(slot as u64),
                1,
                1024,
                1024,
                Vec::new(),
            )
        })
        .collect();
    let segment_key = window
        .register_segment(
            SegmentDraft::new(HistorySource::Own, entries, 8 * 1024, 8 * 1024),
            SegmentPlacement::Tail,
        )
        .expect("segment");
    for slot in 0..8 {
        window
            .index
            .record_tool_result(
                HistorySource::Own,
                format!("pair-{slot:04}"),
                ResultRef::new(
                    HistorySource::Own,
                    format!("{SESSION}:pair-{slot:04}"),
                    "rev",
                ),
                ToolTerminalMeta::new(false, 64 * 1024, "p".repeat(1024)),
            )
            .expect("within budget");
    }
    let before_release = window.index.pending_tool_bytes();
    let segment = window.segment(segment_key).expect("segment").clone();
    let plan = ReleasePlan {
        segment: segment_key,
        source: segment.source.clone(),
        entries: segment.entry_keys(),
        unit_count: segment.unit_count,
        body_bytes: segment.body_bytes,
        reserved_bytes: segment.reserved_bytes,
        slot_range: window.index().slot_range(segment_key),
        before: None,
        after: None,
        before_cursor: None,
        after_cursor: None,
    };
    window.apply_release_plan(&plan).expect("release");
    let after_release = window.index.pending_tool_bytes();
    println!("P05 ResidentIndex 段释放：before={before_release}B after={after_release}B");
    assert!(before_release > 0);
    assert_eq!(after_release, 0, "段释放必须剪枝跨页配对元数据");
    print_process("process(end)");
}
