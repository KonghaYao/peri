use super::AcpEventData;
use super::BridgeState;
use super::EntryStatus;
use super::FoldState;
use super::TuiRenderUnit;
use super::TuiTextChunk;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::serial;

/// P0 regression: a newly submitted prompt must follow every visible entry
/// from the previous turn, even when its terminal event has not arrived yet.
#[test]
#[serial]
fn p0_new_prompt_follows_unarchived_previous_answer() {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q1".into(),
            attachments: Vec::new(),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a1".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q2".into(),
            attachments: Vec::new(),
        },
    );

    let sequence: Vec<_> = VIEW_MODELS
        .state()
        .read()
        .items
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiUserBubble(b) => Some(format!("user:{}", b.text)),
            TuiRenderUnit::TuiAssistantBubble(b) => Some(format!("assistant:{}", b.text)),
            _ => None,
        })
        .collect();
    assert_eq!(sequence, ["user:q1", "assistant:a1", "user:q2"]);

    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let archived: Vec<_> = VIEW_MODELS
        .state()
        .read()
        .items
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiUserBubble(b) => Some(format!("user:{}", b.text)),
            TuiRenderUnit::TuiAssistantBubble(b) => Some(format!("assistant:{}", b.text)),
            _ => None,
        })
        .collect();
    assert_eq!(
        archived, sequence,
        "turn terminal must not reorder visible prompts"
    );
}

/// P0 regression: a prompt arriving while a tool runs must preserve the tool
/// accumulator so its eventual result can still update the visible card.
#[test]
#[serial]
fn p0_new_prompt_preserves_running_tool_result() {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q1".into(),
            attachments: Vec::new(),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "tool-1".into(),
            tool_name: "Read".into(),
            input_summary: "file.rs".into(),
            raw_input: serde_json::json!({"path": "file.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q2".into(),
            attachments: Vec::new(),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "tool-1".into(),
            output_summary: "file contents".into(),
            is_error: false,
            agent_id: None,
        }),
    );

    let snapshot = VIEW_MODELS.state().read().clone();
    let items = &snapshot.items;
    let tool = items.iter().position(
        |vm| matches!(vm, TuiRenderUnit::TuiCollapsedGroup(group) if group.view_models.iter().any(|inner| matches!(inner, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-1" && card.output_summary == "file contents"))),
    );
    let prompt = items
        .iter()
        .position(|vm| matches!(vm, TuiRenderUnit::TuiUserBubble(b) if b.text == "q2"));
    assert!(tool.is_some(), "completed tool result must remain visible");
    assert!(tool < prompt, "new prompt must follow the previous tool");
    assert!(
        matches!(
            prompt
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| items.get(index)),
            Some(TuiRenderUnit::TuiDivider(_))
        ),
        "运行中的工具阻止归档时，分隔线仍应紧贴下一条 prompt"
    );
}

// ── Slice 3：快照后处理流水线（turn divider / todo 摘要）─────────────────

/// [回归测试] 旧 divider 依赖 current_turn 非空，回合结束后会消失并造成闪动。
/// 现在分隔线与下一条 user prompt 一起进入时间轴，完成后仍保留。
#[test]
#[serial]
fn test_snapshot_turn_divider_before_new_user_prompt() {
    let mut state = super::make_fold_test_state();

    // Turn 1：user + answer → TurnDone（committed = [user1, answer1]）
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q1".into(),
            attachments: Vec::new(),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a1".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // Turn 2 流式期间：committed = [user1, answer1, user2]
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q2".into(),
            attachments: Vec::new(),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a2".into(),
            message_id: Some("m2".into()),
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // [user1, answer1, divider, user2, a2]
    assert_eq!(snap.items.len(), 5);
    match &snap.items[2] {
        TuiRenderUnit::TuiDivider(d) => assert_eq!(d.label, None),
        other => panic!("expected TuiDivider at [2], got {other:?}"),
    }
    assert!(
        matches!(&snap.items[3], TuiRenderUnit::TuiUserBubble(_)),
        "divider 应在新 prompt 之前"
    );

    // TurnDone 后 divider 仍随 user2 保留，避免横线闪动。
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 5, "turn 完成后分隔线仍在");
    assert!(matches!(snap.items[2], TuiRenderUnit::TuiDivider(_)));
}

/// 首轮（committed 仅 1 个 prompt）不插 divider；同一 turn 内 prompt↔回复
/// 之间不插 divider。
#[test]
#[serial]
fn test_snapshot_no_divider_for_first_turn_or_inside_turn() {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "q1".into(),
            attachments: Vec::new(),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a1".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    // [user1, a1]——首轮无 divider
    assert_eq!(snap.items.len(), 2);
    assert!(
        snap.items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiDivider(_))),
        "首轮 prompt↔回复 之间不得有 divider"
    );
}

/// §6.9 todo 摘要：活动 turn（PromptRunning）且 TODO_ITEMS 非空时，
/// 摘要行插在 trailing 最终回答之前；回答后无 todo。
#[test]
#[serial]
fn test_snapshot_todo_summary_before_final_answer() {
    let mut state = super::make_fold_test_state();
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![
        crate::kit::message_area::TodoItem {
            status: crate::kit::message_area::TodoStatus::InProgress,
            content: "Running tests".into(),
        },
        crate::kit::message_area::TodoItem {
            status: crate::kit::message_area::TodoStatus::Completed,
            content: "Setup".into(),
        },
    ];

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "final answer".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // [todo_summary, answer]——摘要位于最终回答之前
    assert_eq!(snap.items.len(), 2);
    match &snap.items[0] {
        TuiRenderUnit::TuiTodoSummary(s) => {
            assert!(
                s.text.contains("1/2") && s.text.contains("Running tests"),
                "摘要格式 `1/2 tasks · Running tests`，实际 {:?}",
                s.text
            );
        }
        other => panic!("expected TuiTodoSummary at [0], got {other:?}"),
    }
    assert!(
        matches!(&snap.items[1], TuiRenderUnit::TuiAssistantBubble(_)),
        "最终回答在摘要之后"
    );

    // TurnDone 后（current_turn 清空）→ 无 todo 摘要
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        snap.items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiTodoSummary(_))),
        "回答后无 todo 摘要"
    );
    crate::kit::atoms::TODO_ITEMS.state().write().clear();
}

/// [回归] todo 摘要位于 subagent group 之后时，快照分组缓存的指纹必须包含
/// group 本身；否则 child 内容变化但末尾摘要不变会误命中旧缓存。
#[test]
#[serial]
fn test_snapshot_cache_tracks_subagent_before_todo_summary() {
    let mut state = super::make_fold_test_state();
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
        status: crate::kit::message_area::TodoStatus::InProgress,
        content: "subagent fingerprint regression sentinel".into(),
    }];

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "cache-agent".into(),
            agent_name: "researcher".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "child text".into(),
            message_id: Some("cache-child".into()),
            agent_id: Some("cache-agent".into()),
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "group 后应保留 todo 摘要");
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            assert_eq!(group.agent_id, "cache-agent");
            assert_eq!(
                group.view_models.len(),
                1,
                "child 更新后不得复用启动时的空 group 缓存"
            );
        }
        other => panic!("expected TuiSubAgentGroup at [0], got {other:?}"),
    }

    crate::kit::atoms::TODO_ITEMS.state().write().clear();
}

/// 正文空档分组：空档内相邻工具（含 running/error）合成一张组行，组内成员
/// 保留折叠 pass 给出的单行折叠（可单独点开）；正文出现后成为下一段的边界，
/// 不跨正文合并。
#[test]
#[serial]
fn test_snapshot_tools_grouped_in_body_gap() {
    let mut state = super::make_fold_test_state();

    let start_read = |st: &mut BridgeState, id: &str, path: &str| {
        super::dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: path.into(),
                raw_input: serde_json::json!({"path": path}),
                agent_id: None,
                origin_message_id: None,
                origin_readable: None,
            }),
        );
    };
    let end_tool = |st: &mut BridgeState, id: &str, is_error: bool| {
        super::dispatch_and_notify(
            st,
            &AcpEventData::ToolEnded(TuiToolEnded {
                images: Vec::new(),
                tool_id: id.into(),
                output_summary: "ok".into(),
                is_error,
                agent_id: None,
            }),
        );
    };

    // 相邻成功：t1, t2 各自一张卡片（不再合并为组行）。
    start_read(&mut state, "t1", "a.rs");
    end_tool(&mut state, "t1", false);
    start_read(&mut state, "t2", "b.rs");
    end_tool(&mut state, "t2", false);
    // running：t3 同样独立且单行折叠。
    start_read(&mut state, "t3", "c.rs");
    // error：t4 同样独立且单行折叠（失败由状态符号 + Failed 文案表达）。
    start_read(&mut state, "t4", "d.rs");
    end_tool(&mut state, "t4", true);

    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 1, "同一空档内的工具只占一张组行");
    let group = match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    };
    assert_eq!(group.count, 4);
    assert_eq!(group.failed_count, 1, "error 工具计数在组行保留");
    assert_eq!(group.fold, FoldState::Collapsed, "组默认单行");
    let ids: Vec<&str> = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(t) => t.tool_id.as_str(),
            other => panic!("expected TuiToolCard, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, ["t1", "t2", "t3", "t4"]);
    for vm in crate::kit::tui_render_unit::flatten_units(&snap.items) {
        let TuiRenderUnit::TuiToolCard(card) = vm else {
            unreachable!("遍历目标已在上方断言为工具卡")
        };
        assert_eq!(
            card.fold,
            FoldState::Collapsed,
            "组内工具 {} 保留折叠 pass 的单行折叠（可单独点开）",
            card.tool_id
        );
    }
    assert!(
        crate::kit::tui_render_unit::flatten_units(&snap.items)
            .into_iter()
            .any(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(t) if t.tool_id == "t3" && t.is_running)),
        "running 工具留在组内并保持 running 状态"
    );

    // 正文追加：过程组之后出现正文，组成下一段边界。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "中间正文".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        snap.items.len(),
        2,
        "过程组 + 正文各占一个槽位，不跨正文合并"
    );
    assert!(matches!(
        snap.items.last(),
        Some(TuiRenderUnit::TuiAssistantBubble(_))
    ));
    assert!(matches!(
        &snap.items[0],
        TuiRenderUnit::TuiCollapsedGroup(group) if group.count == 4
    ));
}

/// Skill/Todo 语义卡与普通工具同属过程区：同一空档内合成一张组行。
#[test]
#[serial]
fn test_snapshot_semantic_tools_in_group() {
    let mut state = super::make_fold_test_state();
    let indexed_tool_names = ["Skill", "TodoWrite"].iter().enumerate();
    for (i, name) in indexed_tool_names {
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: format!("k{i}"),
                tool_name: name.to_string(),
                input_summary: "x".into(),
                raw_input: serde_json::json!({"skill": "s", "todos": []}),
                agent_id: None,
                origin_message_id: None,
                origin_readable: None,
            }),
        );
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                images: Vec::new(),
                tool_id: format!("k{i}"),
                output_summary: "ok".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 1, "语义卡同一空档合成一张组行");
    assert!(matches!(
        &snap.items[0],
        TuiRenderUnit::TuiCollapsedGroup(group) if group.count == 2
    ));
    assert_eq!(
        crate::kit::tui_render_unit::flatten_units(&snap.items).len(),
        2,
        "两张语义卡都在组内"
    );
}

// ── Slice 1：空 reasoning 占位（§6.3）+ assistant 时长冻结（§6.2）────────

fn assistant_of(
    snapshot: &ViewModelsSnapshot,
    idx: usize,
) -> &crate::kit::tui_render_unit::TuiAssistantBubble {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiAssistantBubble(b) => b,
        other => panic!("expected TuiAssistantBubble at [{idx}], got {other:?}"),
    }
}

/// §6.3 空 reasoning 占位：仅文本（无 reasoning chunk）流式 → 占位块
/// （text 空、Running、默认 Collapsed 单行）；TurnDone 后翻转 Completed，
/// 仍保持 Collapsed 单行（过程 entry 高度不变）。
#[test]
#[serial]
fn test_empty_reasoning_placeholder_streams_then_folds() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_e1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = super::reasoning_of(&snap, 0);
    assert_eq!(
        r.text, "",
        "无 reasoning chunk → 空占位块（不出现空白 block）"
    );
    assert_eq!(r.status, EntryStatus::Running, "流式中占位块为 Running");
    assert!(r.is_running);
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "running 占位块同样默认单行折叠"
    );

    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let r = super::reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "TurnDone 后空占位块翻转 Completed"
    );
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "completed 仍保持 Collapsed（高度不因终态改变）"
    );
}

/// [R6] 空占位块 hash 跨 rebuild 稳定：流式追加（bubble 重建）→ 状态翻转
/// → 冻结后再次触发快照后处理，hash 保持稳定（秒级）。
#[test]
#[serial]
fn test_empty_reasoning_placeholder_hash_stable_across_rebuild() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a".into(),
            message_id: Some("msg_e2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let running_hash = assistant_of(&snap, 0).content_hash;

    // 流式追加文本——bubble 重建，hash 随内容变化
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "b".into(),
            message_id: Some("msg_e2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let grown_hash = assistant_of(&snap, 0).content_hash;
    assert_ne!(grown_hash, running_hash, "内容变化 hash 必须变化");

    // TurnDone 冻结（fold/status/duration 翻转）——hash 再变一次
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let frozen_hash = assistant_of(&snap, 0).content_hash;
    assert_ne!(frozen_hash, grown_hash, "状态翻转 hash 必须变化");

    // 冻结后快照静态：再次触发快照后处理，hash 秒级稳定（R6）
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnCommitted {
            messages_json: "[]".into(),
            steps: 1,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        assistant_of(&snap, 0).content_hash,
        frozen_hash,
        "冻结后跨 rebuild hash 稳定"
    );
}

/// §6.2 `12.4s`：turn 完成时冻结 assistant 正文时长（镜像 reasoning 冻结
/// 机制——apply_fold_pass 翻转点）；冻结后 hash 秒级稳定。
#[test]
#[serial]
fn test_assistant_duration_frozen_on_turn_done() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "hello".into(),
            message_id: Some("msg_d1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let b = assistant_of(&snap, 0);
    assert!(b.started_at.is_some(), "流式 trailing 段应持有 started_at");
    assert_eq!(b.duration_ms, None, "流式期间无冻结值");
    let running_hash = b.content_hash;

    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let b = assistant_of(&snap, 0);
    assert!(
        b.duration_ms.is_some(),
        "TurnDone 后应冻结 duration_ms（G1：fold pass 翻转点）"
    );
    assert_eq!(b.started_at, None, "冻结后 started_at 置 None（不再增长）");
    let frozen_hash = b.content_hash;
    assert_ne!(
        running_hash, frozen_hash,
        "running→frozen 翻转必须改变 content_hash（frozen 判别位）——冻结落在同一秒时 \
         duration_secs 数值不变，无判别位则按 hash 分片的渲染缓存持续供应无 meta 的旧帧"
    );

    // 冻结后快照静态：再次触发快照后处理，hash 秒级稳定
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnCommitted {
            messages_json: "[]".into(),
            steps: 1,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    let b = assistant_of(&snap, 0);
    assert_eq!(b.content_hash, frozen_hash, "冻结后 hash 稳定");
}

use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};
