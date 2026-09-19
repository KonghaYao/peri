//! [PERF] 增量分组的差分级验证——缓存复用路径必须与「每次全量重建」逐字段等价。
//!
//! 分组结果缓存（`TOOL_GROUP_CACHE`）是**纯函数 memoization**：复用前缀的唯一
//! 理由是「相同输入 ⇒ 相同输出」。本文件用同一串事件跑两条路径：
//!
//! - 热缓存：正常 dispatch（走稳定切点复用 + 后缀重建）；
//! - 冷缓存：每次发布前清空缓存（强制从 0 全量重建）。
//!
//! 两者产出的 `VIEW_MODELS` 必须**逐条结构相等**：`TuiRenderUnit` 派生
//! `PartialEq`，比较全部字段，陈旧但 hash 相同的字段也会被抓到。

use super::*;
use crate::kit::stream_data::{TuiTextChunk, TuiToolEnded, TuiToolStarted};

fn tool_started(id: &str, name: &str) -> AcpEventData {
    AcpEventData::ToolStarted(TuiToolStarted {
        tool_id: id.into(),
        tool_name: name.into(),
        input_summary: format!("{name} input"),
        raw_input: serde_json::json!({ "id": id }),
        agent_id: None,
    })
}

fn tool_ended(id: &str, is_error: bool) -> AcpEventData {
    AcpEventData::ToolEnded(TuiToolEnded {
        tool_id: id.into(),
        output_summary: if is_error { "boom" } else { "ok" }.into(),
        is_error,
        agent_id: None,
    })
}

fn text(t: &str) -> AcpEventData {
    AcpEventData::TextChunk(TuiTextChunk {
        text: t.into(),
        message_id: Some("m1".into()),
        agent_id: None,
    })
}

/// 把快照中**由装配时刻读钟得到**的时长字段归一到同一取值，其余字段原样保留。
///
/// 两条路径的快照取自不同壁钟时刻，而这些字段按 `started_at.elapsed()` 在装配时
/// 计算：折叠 pass 在 phase 离开 `PromptRunning` 时冻结正文/推理时长
/// （`apply_fold_pass`），running 工具卡片的实时时长同样来自 `elapsed()`。相差
/// 几毫秒是取钟时刻差，不是缓存用错输入——`docs/standards/testing.md` 要求测试
/// 不依赖真实时钟，逐值比较这些毫秒值只会得到一条随机失败的用例。缓存正确性由
/// 其余字段承载，它们仍逐值比较：这些字段的**存在性**（`Some`/`None` 即 running
/// 与冻结状态）、`started_at`、`end_tool` 时已冻结的
/// `TuiToolCard::completed_duration_ms`，以及其余全部结构字段。
fn normalize_assembly_clock(unit: &TuiRenderUnit) -> TuiRenderUnit {
    let mut unit = unit.clone();
    match &mut unit {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            bubble.duration_ms = bubble.duration_ms.map(|_| 0);
            if let Some(reasoning) = bubble.reasoning.as_mut() {
                reasoning.duration_ms = reasoning.duration_ms.map(|_| 0);
            }
        }
        TuiRenderUnit::TuiToolCard(card) => {
            card.running_duration_ms = card.running_duration_ms.map(|_| 0);
        }
        TuiRenderUnit::TuiCollapsedGroup(group) => {
            group.view_models = group
                .view_models
                .iter()
                .map(normalize_assembly_clock)
                .collect();
        }
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            group.view_models = group
                .view_models
                .iter()
                .map(normalize_assembly_clock)
                .collect();
        }
        _ => {}
    }
    unit
}

/// 逐事件差分：dispatch（热缓存）→ 取快照 → 清缓存 → 重新发布（全量重建）→ 取快照
/// → 断言逐条相等。清缓存后重新发布会把缓存以**参照结果**填回，故下一个事件仍在
/// 复用路径上（参照结果与热路径等价，正是本测试要证的命题）。
fn assert_incremental_equivalent(state: &mut BridgeState, events: &[AcpEventData]) {
    for (idx, ev) in events.iter().enumerate() {
        dispatch_and_notify(state, ev);
        let incremental: Vec<TuiRenderUnit> = VIEW_MODELS
            .state()
            .read()
            .items
            .iter()
            .map(normalize_assembly_clock)
            .collect();
        crate::kit::acp_events::render::reset_tool_group_cache();
        crate::kit::acp_events::render::push_view_models(state);
        let reference: Vec<TuiRenderUnit> = VIEW_MODELS
            .state()
            .read()
            .items
            .iter()
            .map(normalize_assembly_clock)
            .collect();
        assert_eq!(
            incremental, reference,
            "事件 #{idx}（{ev:?}）的增量快照与全量重建不一致"
        );
    }
}

/// 长会话主形态：工具生命周期 + 归档 + 流式文本交替。覆盖组收口切点
/// （新组接在旧组之后）、段尾可合并卡（新增卡可能续接连串）、文本尾变更。
#[test]
#[serial]
fn test_incremental_group_tool_lifecycle() {
    let mut state = make_fold_test_state();
    let mut events = vec![AcpEventData::PromptSubmitted {
        request_id: Some("r1".into()),
    }];
    for i in 0..8 {
        events.push(tool_started(&format!("t{i}"), "Read"));
        events.push(tool_ended(&format!("t{i}"), false));
        if i % 3 == 2 {
            events.push(text(&format!("step {i} 完成")));
            events.push(AcpEventData::TurnSuspended);
        }
    }
    // 尾部落单卡片（可合并但只有一张，不组）→ 再来一张成组 → 错误卡片续接。
    events.push(tool_started("solo", "Glob"));
    events.push(tool_ended("solo", false));
    events.push(tool_started("pair", "Glob"));
    events.push(tool_ended("pair", false));
    events.push(tool_started("bad", "Bash"));
    events.push(tool_ended("bad", true));
    events.push(text("收尾"));
    events.push(AcpEventData::TurnDone);
    assert_incremental_equivalent(&mut state, &events);
}

/// 失败 lookahead：折叠组之后紧跟 error 连串——组的 `failed_count` 依赖连串长度，
/// 连串增长/变更时必须重建该组（切点不可落在未收口的连串内）。
#[test]
#[serial]
fn test_incremental_group_error_lookahead() {
    let mut state = make_fold_test_state();
    let mut events = vec![AcpEventData::PromptSubmitted {
        request_id: Some("r1".into()),
    }];
    for i in 0..4 {
        events.push(tool_started(&format!("ok{i}"), "Read"));
        events.push(tool_ended(&format!("ok{i}"), false));
    }
    for i in 0..3 {
        events.push(tool_started(&format!("e{i}"), "Bash"));
        events.push(tool_ended(&format!("e{i}"), true));
    }
    // 错误卡片自身变更（重跑成功）→ 必须从组或更早处重建。
    events.push(tool_started("again", "Read"));
    events.push(tool_ended("again", false));
    events.push(text("重试完成"));
    assert_incremental_equivalent(&mut state, &events);
}

/// 焦点免疫与折叠覆盖：辅助输入（FOCUSED_ENTRY / FOLD_OVERRIDES / 语言版本）变化
/// 必须让缓存整体失效——否则会复用旧的组成员或折叠态。
#[test]
#[serial]
fn test_incremental_group_aux_inputs() {
    use crate::kit::atoms::{FOCUSED_ENTRY, FocusedEntry};
    use crate::kit::tui_render_unit::{FoldKey, FoldState};

    let mut state = make_fold_test_state();
    *FOCUSED_ENTRY.state().write() = None;
    let mut events = vec![AcpEventData::PromptSubmitted {
        request_id: Some("r1".into()),
    }];
    for i in 0..4 {
        events.push(tool_started(&format!("t{i}"), "Read"));
        events.push(tool_ended(&format!("t{i}"), false));
    }
    assert_incremental_equivalent(&mut state, &events);

    // 焦点落在组内工具上 → 该卡免疫（不再并入组），须整体重算。
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry {
        slot: 0,
        key: Some(FoldKey::Tool("t1".into())),
    });
    assert_incremental_equivalent(&mut state, &[text("聚焦后")]);

    // 焦点移走 → 恢复自动合并。
    *FOCUSED_ENTRY.state().write() = None;
    assert_incremental_equivalent(&mut state, &[text("焦点移走")]);

    // 组展开覆盖：组的 fold 变化必须反映到快照（缓存键含 Group 覆盖折叠态）。
    let group_key = FoldKey::Group(vec!["t0".into(), "t1".into()]);
    *FOLD_OVERRIDES.state().write() = [(
        FoldKey::Group(vec!["t0".into(), "t1".into(), "t2".into(), "t3".into()]),
        FoldState::Expanded,
    )]
    .into_iter()
    .collect();
    assert_incremental_equivalent(&mut state, &[text("展开后")]);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        snap.items.iter().any(|vm| matches!(
            vm,
            TuiRenderUnit::TuiCollapsedGroup(g) if g.fold == FoldState::Expanded
        )),
        "组覆盖展开态应反映在快照中：{:?}",
        snap.items
    );

    // 非 Tool 焦点键：不参与分组判定，但同样不得破坏复用正确性。
    let _ = group_key;
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry {
        slot: 0,
        key: Some(FoldKey::Group(vec!["t0".into()])),
    });
    assert_incremental_equivalent(&mut state, &[text("焦点在组上")]);
}

/// 段收缩与重建：current_turn 归档后段变短（`TurnDone`）、旧段重现（新 turn 的
/// 相同工具序列）——切点下标必须按新输入重新判定，不能按长度假设。
#[test]
#[serial]
fn test_incremental_group_shrinking_segment() {
    let mut state = make_fold_test_state();
    let mut events = Vec::new();
    for round in 0..3 {
        events.push(AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        });
        for i in 0..2 {
            events.push(tool_started(&format!("r{round}-t{i}"), "Read"));
            events.push(tool_ended(&format!("r{round}-t{i}"), false));
        }
        events.push(AcpEventData::TurnDone);
    }
    assert_incremental_equivalent(&mut state, &events);
}
