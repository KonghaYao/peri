use super::*;

// ── Slice 3：快照后处理流水线（turn divider / todo 摘要 / 工具分组）─────────

/// §6.6 turn 边界 divider：上一 turn 结束后，新 turn 的 prompt 位于 committed
/// 末尾——divider 插在 prompt 之前（committed|current_turn 边界本身是
/// prompt↔回复 的同一 turn 内部，不能用）。
#[test]
#[serial]
fn test_snapshot_turn_divider_before_new_user_prompt() {
    let mut state = make_fold_test_state();

    // Turn 1：user + answer → TurnDone（committed = [user1, answer1]）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "q1".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a1".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // Turn 2 流式期间：committed = [user1, answer1, user2]
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "q2".into() },
    );
    dispatch_and_notify(
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

    // TurnDone 后 current_turn 清空 → divider 消失（仅流式期间存在）
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 4, "turn 完成后无 divider");
}

/// 首轮（committed 仅 1 个 prompt）不插 divider；同一 turn 内 prompt↔回复
/// 之间不插 divider。
#[test]
#[serial]
fn test_snapshot_no_divider_for_first_turn_or_inside_turn() {
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "q1".into() },
    );
    dispatch_and_notify(
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
    let mut state = make_fold_test_state();
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

    dispatch_and_notify(
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
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        snap.items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiTodoSummary(_))),
        "回答后无 todo 摘要"
    );
}

/// §7 工具分组：相邻成功 Generic 工具压成 TuiCollapsedGroup（标题含隐藏数）；
/// running/error/diff-edit 不合并；不跨越 assistant 正文。
#[test]
#[serial]
fn test_snapshot_group_successful_tools() {
    let mut state = make_fold_test_state();

    let start_read = |st: &mut BridgeState, id: &str, path: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: path.into(),
                raw_input: serde_json::json!({"path": path}),
                agent_id: None,
            }),
        );
    };
    let end_tool = |st: &mut BridgeState, id: &str, is_error: bool| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: "ok".into(),
                is_error,
                agent_id: None,
            }),
        );
    };

    // 相邻成功：Read t1, Read t2 → 分组
    start_read(&mut state, "t1", "a.rs");
    end_tool(&mut state, "t1", false);
    start_read(&mut state, "t2", "b.rs");
    end_tool(&mut state, "t2", false);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 1, "两个相邻成功工具 → 1 个分组");
    match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(g) => {
            assert_eq!(g.count, 2);
            assert!(
                g.title.contains("Read 2"),
                "标题含隐藏数，实际 {:?}",
                g.title
            );
            assert_eq!(g.view_models.len(), 2, "隐藏 VM 保留在组内");
        }
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }

    // running 工具不合并（新工具开始 → 分组与 running 分离）
    start_read(&mut state, "t3", "c.rs");
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "running 工具不得并入分组");
    assert!(matches!(
        &snap.items[0],
        TuiRenderUnit::TuiCollapsedGroup(_)
    ));
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.is_running));

    // error 工具不合并——组后**连续相邻** error 计入 failed_count（D2）
    end_tool(&mut state, "t3", true);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "error 工具不得并入分组");
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.is_error));
    match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(g) => {
            assert_eq!(
                g.failed_count, 1,
                "紧邻 error 工具计入失败数（error 仍独立展开，不入组）"
            );
            // [G1] failed_count 纳入 hash——变化必须触发分片缓存重建
            assert_ne!(
                g.content_hash, 0,
                "组 hash 由 recompute_hash 计算（含 failed_count）"
            );
        }
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }

    // 正文打断相邻性：Read + text + Read → 不跨正文分组
    let mut state2 = make_fold_test_state();
    let start2 = |st: &mut BridgeState, id: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: "x".into(),
                raw_input: serde_json::json!({"path": "x"}),
                agent_id: None,
            }),
        );
    };
    start2(&mut state2, "s1");
    dispatch_and_notify(
        &mut state2,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "s1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state2,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "中间正文".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    start2(&mut state2, "s2");
    dispatch_and_notify(
        &mut state2,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "s2".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 3, "正文打断 → 不跨正文分组");
    assert!(matches!(&snap.items[0], TuiRenderUnit::TuiToolCard(_)));
    assert!(matches!(
        &snap.items[1],
        TuiRenderUnit::TuiAssistantBubble(_)
    ));
    assert!(matches!(&snap.items[2], TuiRenderUnit::TuiToolCard(_)));
}

/// Skill/Todo 语义卡不分组（低信息密度才分组，语义卡保留）。
#[test]
#[serial]
fn test_snapshot_group_excludes_semantic_cards() {
    let mut state = make_fold_test_state();
    for (i, name) in ["Skill", "TodoWrite"].iter().enumerate() {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: format!("k{i}"),
                tool_name: name.to_string(),
                input_summary: "x".into(),
                raw_input: serde_json::json!({"skill": "s", "todos": []}),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: format!("k{i}"),
                output_summary: "ok".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "语义卡不参与分组");
    assert!(
        snap.items
            .iter()
            .all(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
    );
}

/// [§7 免疫] 焦点所在工具（`FOCUSED_ENTRY` 的 key）完成也不得并入折叠组。
///
/// 回归（review MED-2/F1）：用户 Alt+Down 聚焦运行中的工具，其完成后若被并入
/// 组——焦点 index 落到组上、展开态丢失（组不可展开且每帧重建）。当前
/// selected entry 按身份键免疫；焦点移走（Esc/导航）后恢复自动合并。
#[test]
#[serial]
fn test_snapshot_group_excludes_focused_tool() {
    use crate::kit::atoms::{FOCUSED_ENTRY, FocusedEntry};
    let mut state = make_fold_test_state();
    *FOCUSED_ENTRY.state().write() = None;
    let start_read = |st: &mut BridgeState, id: &str, path: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: path.into(),
                raw_input: serde_json::json!({"path": path}),
                agent_id: None,
            }),
        );
    };
    let end_tool = |st: &mut BridgeState, id: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: "ok".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    };

    start_read(&mut state, "t1", "a.rs");
    // 用户 Alt+Down 聚焦运行中的 t1（§7：当前 selected entry 免疫）。
    // 分组免疫只读 key（slot 不参与判定——含 slot 会使 key=None 的焦点
    // 移动无谓失效 TOOL_GROUP_CACHE 指纹）。
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry {
        slot: 0,
        key: Some(crate::kit::tui_render_unit::FoldKey::Tool("t1".into())),
    });
    end_tool(&mut state, "t1");
    start_read(&mut state, "t2", "b.rs");
    end_tool(&mut state, "t2");

    // t1 完成（焦点仍在其上）→ 不得并入分组：两个工具保持独立 entry。
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        snap.items.len(),
        2,
        "焦点工具免疫 → 保持独立 entry（不并入折叠组）"
    );
    assert!(matches!(&snap.items[0], TuiRenderUnit::TuiToolCard(t) if t.tool_id == "t1"));
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.tool_id == "t2"));

    // 焦点移走（Esc → 单一事实源清除）→ 下一帧快照恢复自动合并。
    *FOCUSED_ENTRY.state().write() = None;
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "总结".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        matches!(&snap.items[0], TuiRenderUnit::TuiCollapsedGroup(g) if g.count == 2),
        "焦点清除后两个相邻成功工具应并入分组"
    );
}

/// [Slice 3 探针] 真实 E2E 场景：两个相邻成功 Read + 尾部文本 → 应分组。
#[test]
#[serial]
fn test_probe_two_reads_then_text_grouped() {
    let mut state = make_fold_test_state();
    for id in ["t1", "t2"] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: "Cargo.toml".into(),
                raw_input: serde_json::json!({"path": "Cargo.toml"}),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: "line1\nline2".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "已使用 Read 工具读取 Cargo.toml。".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let types: Vec<&str> = snap
        .items
        .iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiCollapsedGroup(g) => {
                eprintln!("GROUP: {:?} count={}", g.title, g.count);
                "group"
            }
            TuiRenderUnit::TuiToolCard(_) => "tool",
            TuiRenderUnit::TuiAssistantBubble(_) => "assistant",
            _ => "other",
        })
        .collect();
    eprintln!("SNAPSHOT: {types:?}");
    assert!(
        snap.items
            .iter()
            .any(|vm| matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_))),
        "两个相邻成功 Read + 尾部文本应分组，实际 {types:?}"
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
/// （text 空、Running、Preview）；TurnDone 后翻转 Completed + Collapsed 单行。
#[test]
#[serial]
fn test_empty_reasoning_placeholder_streams_then_folds() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_e1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.text, "",
        "无 reasoning chunk → 空占位块（不出现空白 block）"
    );
    assert_eq!(r.status, EntryStatus::Running, "流式中占位块为 Running");
    assert!(r.is_running);
    assert_eq!(r.fold, FoldState::Preview, "§7 running 行 → Preview");

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "TurnDone 后空占位块翻转 Completed"
    );
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "§7 completed 行 → Collapsed（收束为单行）"
    );
}

/// [R6] 空占位块 hash 跨 rebuild 稳定：流式追加（bubble 重建）→ 状态翻转
/// → 冻结后再次触发快照后处理，hash 保持稳定（秒级）。
#[test]
#[serial]
fn test_empty_reasoning_placeholder_hash_stable_across_rebuild() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
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
    dispatch_and_notify(
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
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let frozen_hash = assistant_of(&snap, 0).content_hash;
    assert_ne!(frozen_hash, grown_hash, "状态翻转 hash 必须变化");

    // 冻结后快照静态：再次触发快照后处理，hash 秒级稳定（R6）
    dispatch_and_notify(
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
    let mut state = make_fold_test_state();

    dispatch_and_notify(
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

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
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
    dispatch_and_notify(
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
