use super::AcpEventData;
use super::TuiRenderUnit;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::serial;

// ── [G-Diff] Slice 5：含 diff 的 Edit 进正文空档组 + 生产路径 diff 解析 ─────────

/// 含 unified diff 的 Edit 在消息区进入正文空档折叠组，展开后解析并展示 diff
/// （G-Diff 生产路径）。
/// [Fix flaky] 与同文件其它用例共享全局 VIEW_MODELS atom——非 serial 时并行
/// 写读交错会读到对方快照。
#[test]
#[serial]
fn test_edit_with_diff_keeps_diff_in_group() {
    let mut state = super::make_fold_test_state();
    let diff_text = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-old line
+new line
";
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/main.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/main.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "e1".into(),
            output_summary: diff_text.into(),
            is_error: false,
            agent_id: None,
        }),
    );
    // 相邻 Read 工具（可合并组）+ Edit 带 diff
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "r1".into(),
            tool_name: "Read".into(),
            input_summary: "b.rs".into(),
            raw_input: serde_json::json!({"path": "b.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "r1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // 正文空档内相邻工具同组：Edit 与 Read 共享一张组行，展开后按原样显示。
    assert_eq!(snap.items.len(), 1, "相邻工具合成一张折叠组");
    let group = match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    };
    assert_eq!(group.count, 2);
    let edit = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit" => Some(t),
            _ => None,
        })
        .expect("Edit 卡片在组内");
    assert!(
        edit.diff.is_some(),
        "Edit 输出中的 unified diff 被解析（G-Diff 生产路径）"
    );
    let diff = edit
        .diff
        .as_ref()
        .expect("edited diff fixture must carry the parsed diff block");
    assert_eq!(
        diff.path, "src/main.rs",
        "path hint 来自 raw_input.file_path"
    );
    assert_eq!(diff.hunks.len(), 1);
    let change_lines: Vec<_> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| {
            matches!(
                l.kind,
                crate::kit::tui_render_unit::TuiHunkLineKind::Add
                    | crate::kit::tui_render_unit::TuiHunkLineKind::Del
            )
        })
        .collect();
    assert_eq!(change_lines.len(), 2, "+1 −1");
}

/// 非 diff 输出（Edit 结果不含 unified diff）→ diff=None；相邻工具同一组行，
/// 归档前后组形状不变。
/// [Fix flaky] 共享全局 VIEW_MODELS atom——非 serial 时与 serial 测试
/// 并行写读交错（serial_test 只互斥 serial 测试之间）。
#[test]
#[serial]
fn test_edit_plain_output_same_group_across_archive() {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/x.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/x.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "e1".into(),
            output_summary: "Replaced text in src/x.rs".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e2".into(),
            tool_name: "Write".into(),
            input_summary: "src/y.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/y.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "e2".into(),
            output_summary: "Wrote 3 lines".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    // 两个无 diff 的相邻成功工具同一空档 → 一张组行。
    assert_eq!(snap.items.len(), 1, "相邻工具合成一张折叠组");
    let group = match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    };
    assert_eq!(group.count, 2);
    let names: Vec<_> = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(card) => Some(card.tool_name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["Edit", "Write"], "组内保持调用顺序");
    for card in crate::kit::tui_render_unit::flatten_units(&snap.items) {
        let TuiRenderUnit::TuiToolCard(card) = card else {
            panic!("expected TuiToolCard, got {card:?}")
        };
        assert_eq!(
            card.fold,
            crate::kit::tui_render_unit::FoldState::Collapsed,
            "组内工具保留折叠 pass 的单行折叠（可单独点开）"
        );
    }

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnCommitted {
            messages_json: "[]".into(),
            steps: 2,
        },
    );
    let committed_checkpoint = VIEW_MODELS.state().read().clone();
    assert_eq!(committed_checkpoint.items.len(), 1);
    assert!(
        matches!(
            &committed_checkpoint.items[0],
            TuiRenderUnit::TuiCollapsedGroup(group) if group.count == 2
        ),
        "TurnCommitted 检查点保持同一组"
    );

    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let final_snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(final_snapshot.items.len(), 1);
    assert!(
        matches!(
            &final_snapshot.items[0],
            TuiRenderUnit::TuiCollapsedGroup(group) if group.count == 2
        ),
        "TurnDone 归档到 committed 后组形状稳定"
    );
}

/// [Slice 5] 真实摘要路径：Edit 输出 `Added 2 lines to P`（真实工具形态，
/// 无 unified diff）→ 摘要 fallback 解析出 diff 块（adds=2）→ 组内卡片保留 diff。
#[test]
#[serial]
fn test_edit_with_real_summary_diff_keeps_diff_in_group() {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "s1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/s.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/s.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "s1".into(),
            output_summary: "Added 2 lines to src/s.rs".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let edit = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(c) if c.tool_id == "s1" => Some(c.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected TuiToolCard s1, got {:?}", snap.items));
    let diff = edit
        .diff
        .expect("真实摘要应解析出 diff 块（G-Diff fallback）");
    assert_eq!(diff.path, "src/s.rs", "path hint 来自 raw_input.file_path");
    assert!(diff.hunks.is_empty(), "摘要块无 hunk 行");
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&diff);
    assert_eq!((adds, dels), (2, 0), "摘要计数进入顶层字段");

    // 相邻成功 Read + 带 diff 的 Edit → 各自独立 entry
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "s2".into(),
            tool_name: "Read".into(),
            input_summary: "src/r.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/r.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            tool_id: "s2".into(),
            output_summary: "line1\nline2".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let has_edit_tool = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .any(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit" && t.diff.is_some()));
    assert!(has_edit_tool, "带 diff 的 Edit 在组内保持 diff 块");
    assert_eq!(snap.items.len(), 1, "Edit 与 Read 同一组行");
    let group = match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    };
    assert_eq!(group.count, 2);
}

/// [Slice 5] 真实摘要同行数替换（`Replaced 1 line to P`，middleware 新形态）
/// → 解析出 diff 块（adds=dels=1）→ 组内卡片展示变更摘要。
#[test]
#[serial]
fn test_edit_same_line_replacement_with_count_in_group() {
    let mut state = super::make_fold_test_state();
    for (id, output) in [
        ("r1", "Replaced 1 line to src/a.rs"),
        ("r2", "Replaced 1 line to src/b.rs"),
    ] {
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Edit".into(),
                input_summary: format!("src/{}.rs", id),
                raw_input: serde_json::json!({"file_path": format!("src/{}.rs", id)}),
                agent_id: None,
                origin_message_id: None,
                origin_readable: None,
            }),
        );
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                images: Vec::new(),
                tool_id: id.into(),
                output_summary: output.into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 1, "相邻 Edit 合成一张折叠组");
    let group = match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    };
    assert_eq!(group.count, 2);
    for item in crate::kit::tui_render_unit::flatten_units(&snap.items) {
        match item {
            TuiRenderUnit::TuiToolCard(c) => {
                let diff = c.diff.as_ref().expect("同行数替换摘要应解析出 diff 块");
                let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(diff);
                assert_eq!((adds, dels), (1, 1), "替换 1 行 → +1 −1");
            }
            other => panic!("expected TuiToolCard, got {other:?}"),
        }
    }
}

use crate::kit::atoms::VIEW_MODELS;
