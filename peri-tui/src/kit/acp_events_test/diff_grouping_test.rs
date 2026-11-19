use super::AcpEventData;
use super::TuiRenderUnit;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::serial;

// ── [G-Diff] Slice 5：含 diff 的 Edit 不入组 + 生产路径 diff 解析 ─────────

/// 含 unified diff 的 Edit 在消息区保留独立卡片并解析 diff（G-Diff 生产路径）。
/// [Fix flaky] 与同文件其它用例共享全局 VIEW_MODELS atom——非 serial 时并行
/// 写读交错会读到对方快照。
#[test]
#[serial]
fn test_edit_with_diff_stays_individual() {
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
            tool_id: "r1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // Read 单独成组（run_len >= 2 才压缩——1 个 Read 不组）；Edit 保持独立卡片
    let edit = snap
        .items
        .iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit" => Some(t),
            _ => None,
        })
        .expect("Edit 卡片独立存在");
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
    // 工具不再聚合：含 diff 的 Edit 与相邻 Read 各自独立 entry。
    assert_eq!(snap.items.len(), 2, "Edit 与 Read 各自独立");
    assert!(
        snap.items
            .iter()
            .all(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_))),
        "不存在聚合组行"
    );
}

/// 非 diff 输出（Edit 结果不含 unified diff）→ diff=None；工具仍各自独立，
/// 归档前后都保持逐条 entry（不聚合）。
/// [Fix flaky] 共享全局 VIEW_MODELS atom——非 serial 时与 serial 测试
/// 并行写读交错（serial_test 只互斥 serial 测试之间）。
#[test]
#[serial]
fn test_edit_plain_output_stays_individual_across_archive() {
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
            tool_id: "e2".into(),
            output_summary: "Wrote 3 lines".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    // 两个无 diff 的相邻成功工具各自独立成 entry。
    assert_eq!(snap.items.len(), 2, "相邻工具各自独立");
    assert!(matches!(&snap.items[0], TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit"));
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Write"));
    for vm in snap.items.iter() {
        let TuiRenderUnit::TuiToolCard(card) = vm else {
            panic!("expected TuiToolCard, got {vm:?}")
        };
        assert_eq!(
            card.fold,
            crate::kit::tui_render_unit::FoldState::Collapsed,
            "工具默认单行折叠"
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
    assert_eq!(committed_checkpoint.items.len(), 2);
    assert!(
        committed_checkpoint
            .items
            .iter()
            .all(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_))),
        "TurnCommitted 检查点保持独立工具卡"
    );

    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let final_snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(final_snapshot.items.len(), 2);
    assert!(
        final_snapshot
            .items
            .iter()
            .all(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_))),
        "TurnDone 归档到 committed 后仍保持独立工具卡"
    );
}

/// [Slice 5] 真实摘要路径：Edit 输出 `Added 2 lines to P`（真实工具形态，
/// 无 unified diff）→ 摘要 fallback 解析出 diff 块（adds=2）→ 独立卡片。
#[test]
#[serial]
fn test_edit_with_real_summary_diff_stays_individual() {
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
            tool_id: "s1".into(),
            output_summary: "Added 2 lines to src/s.rs".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let edit = match &snap.items[0] {
        TuiRenderUnit::TuiToolCard(c) => c.clone(),
        other => panic!("expected TuiToolCard, got {other:?}"),
    };
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
            tool_id: "s2".into(),
            output_summary: "line1\nline2".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let has_edit_tool = snap.items.iter().any(
        |vm| matches!(vm, TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit" && t.diff.is_some()),
    );
    assert!(has_edit_tool, "带 diff 的 Edit 保持独立卡片");
    assert_eq!(snap.items.len(), 2, "Edit 与 Read 各自独立（无聚合组）");
}

/// [Slice 5] 真实摘要同行数替换（`Replaced 1 line to P`，middleware 新形态）
/// → 解析出 diff 块（adds=dels=1）→ 各自独立展示变更摘要。
#[test]
#[serial]
fn test_edit_same_line_replacement_with_count_stays_individual() {
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
                tool_id: id.into(),
                output_summary: output.into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "带 diff 计数的相邻 Edit 各自独立");
    for item in &snap.items {
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
