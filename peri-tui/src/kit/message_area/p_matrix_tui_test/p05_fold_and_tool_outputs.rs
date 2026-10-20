//! P05：频繁折叠 + 长工具输出 + 跨页工具配对（自 `p_matrix_tui_test` 拆分）。

use super::*;

use crate::kit::history_window::{
    PENDING_TOOL_ENDS_BYTES_MAX, PENDING_TOOL_ENDS_LIMIT, PersistentEntryKey, ReleasePlan,
    ReleaseReason, release_window_plans,
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
const P05_PENDING_TOOL_BYTES: usize = PENDING_TOOL_ENDS_BYTES_MAX;
const P05_PENDING_TOOL_RESULTS: usize = PENDING_TOOL_ENDS_LIMIT;

/// P05：60 页长工具输出 + 每页折叠切换；跨页工具终点由轻量摘要承接。
///
/// 记录历史正文、渲染缓存、折叠投影与 PENDING_TOOL_ENDS 的峰值；释放只精准
/// 清理被释放条目对应的未配对终点，其他段的结果保留（受统一预算约束）。
#[test]
#[ignore = "P 矩阵测量（频繁折叠/长工具输出/跨页配对）"]
#[serial]
fn p05_fold_and_long_tool_outputs_reclaim_caches_and_pairing() {
    let window_limits = WindowLimits {
        body_bytes: P05_BODY_LIMIT,
        reserved_bytes: 1024 * 1024,
        page_target_bytes: 256 * 1024,
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

    // 强制一次淘汰（纯文本大页，不新增未配对终点），验证释放事务按条目清理。
    // 未配对终点压到固定上限（条数/字节均由 history_window::budget 统一）。
    for slot in 0..400 {
        crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
            &format!("stress-tool-{slot:03}"),
            PersistentEntryKey::own(format!("{SESSION}:stress-{slot:03}")),
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
    // 驻留段（真实业务路径，不是测试后门）。释放只清理该段条目对应的暂存终点，
    // 其他段/未驻留条目的终点必须保留。
    state.window.set_anchor(None);
    if let Some(segment_key) = state.window.segments().keys().next().copied() {
        let segment = state.window.segment(segment_key).expect("segment").clone();
        let bound_key = segment
            .entries
            .first()
            .expect("segment has entries")
            .key
            .clone();
        crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
            "p05-bound-tool",
            bound_key,
            &"b".repeat(64),
            false,
        );
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
        assert!(
            crate::kit::acp_events::history_window::pending_tool_end_entry_for_tests(
                "p05-bound-tool"
            )
            .is_none(),
            "被释放段的暂存终点必须精准清理"
        );
        push_view_models(&mut state);
    }
    let (pending_after_release_entries, pending_after_release_bytes) =
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint();
    assert!(
        pending_after_release_entries > 0,
        "其他段/未驻留条目的暂存终点不能被释放事务误清"
    );
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
    assert_budget(
        "pending_after_release_entries",
        pending_after_release_entries,
        P05_PENDING_TOOL_RESULTS,
    );
    assert_budget(
        "pending_after_release_bytes",
        pending_after_release_bytes,
        P05_PENDING_TOOL_BYTES,
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

    print_process("process(end)");
}
