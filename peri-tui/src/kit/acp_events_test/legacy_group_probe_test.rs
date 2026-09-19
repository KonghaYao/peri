//! [EPHEMERAL] 旧分组实现（HEAD 版本）的等价性与成本对照。
//!
//! 目的有两个：
//!
//! 1. **等价性**：旧实现（`git show HEAD:peri-tui/src/kit/acp_events/render.rs` 的
//!    `group_successful_tools`）与新实现在同一输入序列上必须产出**逐条字段相等**的
//!    结果——`group_incremental_test.rs` 的 warm/cold 差分只证明新实现内部自洽，
//!    不覆盖与旧实现的差异；
//! 2. **成本基线**：给出真正的修复前数字。`perf_probe_test.rs` 的 `COLD` 列是新算法
//!    的全量重建路径，**不是**旧实现的实测值。
//!
//! 保真要点：调用前保留一份输入的 `clone()`，使节点引用计数 ≥ 2——生产里
//! `state.committed` 与上一个 `VIEW_MODELS` 快照都在共享这些节点，缺了这一步
//! 旧实现的 `remove`/`insert` 会走独占路径、被严重低估。
//!
//! 运行：`cargo test -p peri-tui --release --lib legacy_group_probe -- --ignored --nocapture`

use crate::kit::atoms::FOLD_OVERRIDES;
use crate::kit::tui_render_unit::{
    FoldKey, FoldState, TuiCollapsedGroup, TuiDivider, TuiRenderUnit, TuiToolPresentation,
    fold_state_code, tui_hash_combine, tui_hash_str,
};

// ---------------------------------------------------------------------------
// 旧实现（HEAD 逐字迁移，仅重命名 + 独立缓存）
// ---------------------------------------------------------------------------

/// 流式 bubble 的内容不进指纹：段末 bubble 的内容每 token 变化。
const LEGACY_TRAILING_BUBBLE_MARKER: u64 = 0x5EAB_1E5E;
/// `TuiToolCard` 的指纹变体码。
const LEGACY_TOOL_VARIANT_CODE: u64 = 3;

struct LegacyToolGroupCache {
    fingerprint: u64,
    grouped: im::Vector<TuiRenderUnit>,
    has_trailing_bubble: bool,
}

static LEGACY_TOOL_GROUP_CACHE: std::sync::Mutex<Option<LegacyToolGroupCache>> =
    std::sync::Mutex::new(None);

fn legacy_group_input_fingerprint(segment: &im::Vector<TuiRenderUnit>) -> (u64, bool) {
    use std::hash::{Hash, Hasher};
    let mut h: u64 = 0;
    let last = segment.len().saturating_sub(1);
    for (i, vm) in segment.iter().enumerate() {
        let is_trailing_bubble = i == last && matches!(vm, TuiRenderUnit::TuiAssistantBubble(_));
        let entry_hash = if is_trailing_bubble {
            LEGACY_TRAILING_BUBBLE_MARKER
        } else {
            match vm {
                TuiRenderUnit::TuiToolCard(t) => {
                    let mut eh = tui_hash_combine(LEGACY_TOOL_VARIANT_CODE, vm.content_hash());
                    eh = tui_hash_combine(eh, tui_hash_str(&t.tool_id));
                    eh = tui_hash_combine(eh, tui_hash_str(&t.tool_name));
                    let flags = u64::from(t.is_running)
                        | (u64::from(t.is_error) << 1)
                        | (u64::from(t.diff.is_some()) << 2)
                        | (u64::from(t.user_modified) << 3)
                        | (u64::from(matches!(t.presentation, TuiToolPresentation::Generic)) << 4);
                    tui_hash_combine(eh, flags)
                }
                other => {
                    let code = match other {
                        TuiRenderUnit::TuiUserBubble(_) => 1,
                        TuiRenderUnit::TuiAssistantBubble(_) => 2,
                        TuiRenderUnit::TuiSystemNote(_) => 4,
                        TuiRenderUnit::TuiSystemReminder(_) => 10,
                        TuiRenderUnit::TuiSubAgentGroup(_) => 5,
                        TuiRenderUnit::TuiCollapsedGroup(_) => 6,
                        TuiRenderUnit::TuiDivider(_) => 7,
                        TuiRenderUnit::TuiAskUserBlock(_) => 8,
                        TuiRenderUnit::TuiTodoSummary(_) => 9,
                        TuiRenderUnit::TuiToolCard(_) => unreachable!(),
                    };
                    tui_hash_combine(code, other.content_hash())
                }
            }
        };
        h = tui_hash_combine(h, entry_hash);
    }
    h = tui_hash_combine(h, segment.len() as u64);
    let mut fh = std::collections::hash_map::DefaultHasher::new();
    let focus_state = crate::kit::atoms::FOCUSED_ENTRY.state();
    focus_state
        .read()
        .as_ref()
        .and_then(|f| f.key.as_ref())
        .hash(&mut fh);
    h = tui_hash_combine(h, fh.finish());
    let mut oh = std::collections::hash_map::DefaultHasher::new();
    let overrides_state = FOLD_OVERRIDES.state();
    let overrides_guard = overrides_state.read();
    let mut group_overrides: Vec<_> = overrides_guard
        .iter()
        .filter_map(|(key, fold)| match key {
            FoldKey::Group(ids) => Some((ids, fold_state_code(*fold))),
            _ => None,
        })
        .collect();
    group_overrides.sort_by_key(|(ids, _)| *ids);
    group_overrides.hash(&mut oh);
    h = tui_hash_combine(h, oh.finish());
    let has_trailing_bubble = matches!(segment.back(), Some(TuiRenderUnit::TuiAssistantBubble(_)));
    (h, has_trailing_bubble)
}

/// 旧 `group_successful_tools`：整段指纹 + 逆序 `remove`/`insert` 就地改写。
fn group_successful_tools_legacy(items: &mut im::Vector<TuiRenderUnit>, start: usize) {
    let mut segment = items.split_off(start);
    let (fingerprint, has_trailing_bubble) = legacy_group_input_fingerprint(&segment);
    let cache = LEGACY_TOOL_GROUP_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_ref()
        && c.fingerprint == fingerprint
        && c.has_trailing_bubble == has_trailing_bubble
    {
        let mut grouped = c.grouped.clone();
        if has_trailing_bubble
            && !grouped.is_empty()
            && let Some(current_last) = segment.pop_back()
        {
            grouped.set(grouped.len() - 1, current_last);
        }
        items.append(grouped);
        return;
    }
    drop(cache);

    let focus_state = crate::kit::atoms::FOCUSED_ENTRY.state();
    let focused_entry = focus_state.read();
    let focused = focused_entry
        .as_ref()
        .and_then(|f| f.key.as_ref())
        .and_then(|k| match k {
            FoldKey::Tool(id) => Some(id.as_str()),
            _ => None,
        });

    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..segment.len() {
        let mergeable = matches!(
            segment.get(i),
            Some(TuiRenderUnit::TuiToolCard(t))
                if !t.is_running && !t.is_error && t.diff.is_none()
                    && matches!(t.presentation, TuiToolPresentation::Generic)
                    && !t.user_modified
                    && focused != Some(t.tool_id.as_str())
        );
        match (mergeable, run_start) {
            (true, None) => run_start = Some(i),
            (true, Some(_)) => {}
            (false, Some(s)) => {
                runs.push((s, i));
                run_start = None;
            }
            (false, None) => {}
        }
    }
    if let Some(s) = run_start {
        runs.push((s, segment.len()));
    }

    for (run_start, run_end) in runs.into_iter().rev() {
        let run_len = run_end - run_start;
        if run_len < 2 {
            continue;
        }
        let mut failed_count: u32 = 0;
        for i in run_end..segment.len() {
            let is_error = matches!(
                segment.get(i),
                Some(TuiRenderUnit::TuiToolCard(t)) if t.is_error
            );
            if is_error {
                failed_count += 1;
            } else {
                break;
            }
        }
        let mut names: Vec<(String, u32)> = Vec::new();
        let mut hidden_vms: Vec<TuiRenderUnit> = Vec::with_capacity(run_len);
        for i in run_start..run_end {
            if let Some(TuiRenderUnit::TuiToolCard(t)) = segment.get(i) {
                let display = crate::kit::tool_display::format_tool_name(&t.tool_name);
                match names.iter_mut().find(|(n, _)| *n == display) {
                    Some((_, c)) => *c += 1,
                    None => names.push((display, 1)),
                }
                hidden_vms.push(segment.get(i).cloned().unwrap());
            }
        }
        let title = names
            .into_iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        let group_key = FoldKey::Group(
            hidden_vms
                .iter()
                .filter_map(|vm| match vm {
                    TuiRenderUnit::TuiToolCard(t) => Some(t.tool_id.clone()),
                    _ => None,
                })
                .collect(),
        );
        let fold = FOLD_OVERRIDES
            .state()
            .read()
            .get(&group_key)
            .copied()
            .unwrap_or(FoldState::Collapsed);
        let mut group = TuiCollapsedGroup {
            title,
            count: run_len as u32,
            failed_count,
            view_models: hidden_vms,
            fold,
            content_hash: 0,
        };
        group.recompute_hash();
        for i in (run_start..run_end).rev() {
            segment.remove(i);
        }
        segment.insert(run_start, TuiRenderUnit::TuiCollapsedGroup(group));
    }

    *LEGACY_TOOL_GROUP_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(LegacyToolGroupCache {
        fingerprint,
        grouped: segment.clone(),
        has_trailing_bubble,
    });
    items.append(segment);
}

// ---------------------------------------------------------------------------
// 对照
// ---------------------------------------------------------------------------

fn reset_legacy_cache() {
    *LEGACY_TOOL_GROUP_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

/// 模拟生产共享：调用前多持一份 `clone()`（`state.committed` / 上一个快照）。
fn call_legacy(v: &mut im::Vector<TuiRenderUnit>) {
    let keep = v.clone();
    group_successful_tools_legacy(v, 0);
    drop(keep);
}

fn call_new(v: &mut im::Vector<TuiRenderUnit>) {
    let keep = v.clone();
    crate::kit::acp_events::render::regroup_at_start(v);
    drop(keep);
}

fn divider(i: usize) -> TuiRenderUnit {
    TuiRenderUnit::TuiDivider(TuiDivider {
        label: Some(format!("d{i}")),
        content_hash: i as u64,
    })
}

/// 同一演化序列跑两遍（逐条断言等价）后计时。
fn compare(
    label: &str,
    base: &im::Vector<TuiRenderUnit>,
    iterations: usize,
    mutate: &mut dyn FnMut(&mut im::Vector<TuiRenderUnit>, usize),
) {
    // 1) 等价性——每步都比较
    {
        let mut a = base.clone();
        let mut b = base.clone();
        reset_legacy_cache();
        crate::kit::acp_events::render::reset_tool_group_cache();
        for i in 0..iterations {
            mutate(&mut a, i);
            mutate(&mut b, i);
            call_legacy(&mut a);
            call_new(&mut b);
            assert_eq!(a, b, "{label}: 第 {i} 步新旧输出不一致");
        }
    }

    // 2) 计时（各自全新缓存）
    let iters = iterations as f64;
    let mut a = base.clone();
    reset_legacy_cache();
    let t0 = std::time::Instant::now();
    for i in 0..iterations {
        mutate(&mut a, i);
        call_legacy(&mut a);
    }
    let legacy_us = t0.elapsed().as_secs_f64() * 1e6 / iters;

    let mut b = base.clone();
    crate::kit::acp_events::render::reset_tool_group_cache();
    let t1 = std::time::Instant::now();
    for i in 0..iterations {
        mutate(&mut b, i);
        call_new(&mut b);
    }
    let new_us = t1.elapsed().as_secs_f64() * 1e6 / iters;

    println!(
        "LEGACY {label} n={} iters={iterations} legacy={legacy_us:>8.1} new={new_us:>7.1} \
         speedup={:.1}x  (us/call)",
        base.len(),
        legacy_us / new_us,
    );
}

#[test]
#[ignore = "定向测量，手动运行：cargo test -p peri-tui --release --lib legacy_group_probe -- --ignored --nocapture"]
fn legacy_group_probe_equivalence_and_cost() {
    for units in [200usize, 500, 1000] {
        // 用真实 handler 造 committed：每轮 ToolStarted+ToolEnded → TurnSuspended 归档。
        let mut state = super::perf_probe_test::build_state(units);
        let base = std::mem::take(&mut state.committed);
        println!("--- base={} units (含可合并 runs) ---", base.len());

        // A. 稳态重复（同一输入反复推送）——旧实现走整段指纹命中路径。
        compare("A_steady", &base, 200, &mut |_, _| {});

        // B. 尾部追加不可合并条目（divider）——旧实现整段指纹失效 → 全量 COW 重建。
        compare("B_tail_divider", &base, 100, &mut |v, i| {
            v.push_back(divider(i + 10_000));
        });
    }
}
