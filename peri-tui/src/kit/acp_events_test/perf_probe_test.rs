//! [EPHEMERAL] push_view_models 定向测量——单次调用成本随 committed 规模与事件形态的变化。
//!
//! 每个 case 使用**全新 state**，避免 case 之间相互污染。
//! 每个 case 跑两遍：热缓存（增量复用）与冷缓存（每次都清空 `TOOL_GROUP_CACHE`，
//! 等价于旧实现的「每事件全量重建」），用同一 state 形状对比成本差。
//! 运行：`cargo test -p peri-tui --lib perf_probe -- --ignored --nocapture`

use super::*;
use crate::kit::stream_data::{TuiTextChunk, TuiToolEnded, TuiToolStarted};

fn tool_started(i: usize) -> AcpEventData {
    AcpEventData::ToolStarted(TuiToolStarted {
        tool_id: format!("t{i}"),
        tool_name: "Bash".into(),
        input_summary: "echo hi".into(),
        raw_input: serde_json::json!({"command": "echo hi"}),
        agent_id: None,
    })
}

fn tool_ended(i: usize) -> AcpEventData {
    AcpEventData::ToolEnded(TuiToolEnded {
        tool_id: format!("t{i}"),
        output_summary: "ok".into(),
        is_error: false,
        agent_id: None,
    })
}

/// 用真实 handler 造 committed 数据：每轮 ToolStarted+ToolEnded → TurnSuspended 归档。
pub(super) fn build_state(units: usize) -> BridgeState {
    let mut state = make_fold_test_state();
    crate::kit::acp_events::render::reset_tool_group_cache();
    let mut i = 0usize;
    while state.committed.len() < units {
        state.phase = SessionPhase::PromptRunning;
        dispatch_and_notify(&mut state, &tool_started(i));
        dispatch_and_notify(&mut state, &tool_ended(i));
        dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
        i += 1;
    }
    state
}

/// 冷缓存模式：清空分组缓存 → 下一次分组从 0 全量重建。
fn reset_if(cold: bool) {
    if cold {
        crate::kit::acp_events::render::reset_tool_group_cache();
    }
}

fn probe(
    label: &str,
    mut state: BridgeState,
    iterations: usize,
    cold: bool,
    step: &mut dyn FnMut(&mut BridgeState, bool),
) {
    let committed = state.committed.len();
    for _ in 0..10 {
        step(&mut state, cold);
    }
    crate::kit::acp_bridge::reset_perf_counters();
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        step(&mut state, cold);
    }
    let elapsed = start.elapsed();
    let c = crate::kit::acp_bridge::perf_counters();
    println!(
        "PROBE {}{label} committed={committed} after={} n={iterations} us_per_call={:.1} \
         full_reuse={:.2} rebuilds={:.2} copied={:.2} rebuilt={:.2} fold_writes={:.2}",
        if cold { "COLD " } else { "" },
        state.committed.len(),
        elapsed.as_secs_f64() * 1e6 / iterations as f64,
        c.group_full_reuse as f64 / iterations as f64,
        c.group_rebuilds as f64 / iterations as f64,
        c.group_copied_units as f64 / iterations as f64,
        c.group_rebuilt_units as f64 / iterations as f64,
        c.fold_pass_writes as f64 / iterations as f64,
    );
    let n = iterations as f64 * 1000.0;
    println!(
        "  STAGES us/call: assemble={:.1} fold={:.1} todo={:.1} group={:.1} write={:.1} sum={:.1}",
        c.stage_assemble_ns as f64 / n,
        c.stage_fold_ns as f64 / n,
        c.stage_todo_ns as f64 / n,
        c.stage_group_ns as f64 / n,
        c.stage_write_ns as f64 / n,
        (c.stage_assemble_ns
            + c.stage_fold_ns
            + c.stage_todo_ns
            + c.stage_group_ns
            + c.stage_write_ns) as f64
            / n,
    );
}

/// 同一 case 的冷/热两遍（各自全新 state）。
fn both(
    label: &str,
    units: usize,
    iterations: usize,
    step: &mut dyn FnMut(&mut BridgeState, bool),
) {
    probe(label, build_state(units), iterations, false, step);
    probe(label, build_state(units), iterations, true, step);
}

#[test]
#[serial]
#[ignore = "定向测量，手动运行：cargo test -p peri-tui --lib perf_probe -- --ignored --nocapture"]
fn perf_probe_push_view_models() {
    for units in [50usize, 200, 500, 1000] {
        println!("--- committed ≈ {units} ---");

        // A. 稳态重复 push（无任何状态变化）——应全量复用（rebuilt=0）
        both("A_steady_push", units, 200, &mut |s, cold| {
            reset_if(cold);
            super::super::render::push_view_models(s);
        });

        // B. 工具卡新增（每事件新增一张 running 卡，不归档）
        let mut c = 0usize;
        both("B_tool_started", units, 100, &mut |s, cold| {
            c += 1;
            reset_if(cold);
            dispatch_and_notify(s, &tool_started(10_000 + c));
        });

        // C. 流式文本（每事件 +16 字符）
        both("C_text_chunk16", units, 200, &mut |s, cold| {
            s.phase = SessionPhase::PromptRunning;
            reset_if(cold);
            dispatch_and_notify(
                s,
                &AcpEventData::TextChunk(TuiTextChunk {
                    text: "0123456789abcdef".into(),
                    message_id: Some("m1".into()),
                    agent_id: None,
                }),
            );
        });

        // C2. 只跑事件处理、不发布——分离 handler 成本与发布（push_view_models）成本
        let mut c2 = 0usize;
        both("C2_text_handler_only", units, 200, &mut |s, _| {
            c2 += 1;
            s.phase = SessionPhase::PromptRunning;
            let _ = super::super::dispatch_for_bridge(
                s,
                &AcpEventData::TextChunk(TuiTextChunk {
                    text: "0123456789abcdef".into(),
                    message_id: Some("m1".into()),
                    agent_id: None,
                }),
            );
        });

        // D. 工具全生命周期（Started → Ended → 归档）——长会话最真实的事件形状
        let mut c = 0usize;
        both("D_tool_lifecycle", units, 60, &mut |s, cold| {
            c += 1;
            s.phase = SessionPhase::PromptRunning;
            reset_if(cold);
            dispatch_and_notify(s, &tool_started(20_000 + c));
            reset_if(cold);
            dispatch_and_notify(s, &tool_ended(20_000 + c));
            reset_if(cold);
            dispatch_and_notify(s, &AcpEventData::TurnSuspended);
        });

        // E. 每次 ReAct 迭代边界的 TurnCommitted（goal 自驱路径，不带状态变化）
        both("E_turn_committed", units, 200, &mut |s, cold| {
            reset_if(cold);
            dispatch_and_notify(
                s,
                &AcpEventData::TurnCommitted {
                    messages_json: "[]".into(),
                    steps: 3,
                },
            );
        });
    }
}
