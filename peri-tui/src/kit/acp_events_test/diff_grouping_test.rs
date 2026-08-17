use super::*;

// ── [G-Diff] Slice 5：含 diff 的 Edit 不入组 + 生产路径 diff 解析 ─────────

/// §7「不得合并含 diff 的 edit」：Edit 输出含 unified diff（解析成功）→
/// 独立展开渲染，不并入 TuiCollapsedGroup（`group_successful_tools` 的
/// `t.diff.is_none()` 守卫自动生效）。
/// [Fix flaky] 与 `test_edit_plain_output_grouped_normally` 等共享全局
/// VIEW_MODELS atom——非 serial 时并行写读交错会读到对方快照。
#[test]
#[serial]
fn test_edit_with_diff_not_grouped() {
    let mut state = make_fold_test_state();
    let diff_text = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-old line
+new line
";
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/main.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/main.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "e1".into(),
            output_summary: diff_text.into(),
            is_error: false,
            agent_id: None,
        }),
    );
    // 相邻 Read 工具（可合并组）+ Edit 带 diff
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "r1".into(),
            tool_name: "Read".into(),
            input_summary: "b.rs".into(),
            raw_input: serde_json::json!({"path": "b.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
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
        .expect("Edit 卡片独立存在（未并入分组）");
    assert!(
        edit.diff.is_some(),
        "Edit 输出中的 unified diff 被解析（G-Diff 生产路径）"
    );
    let diff = edit.diff.as_ref().unwrap();
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
    // 组内不得出现 Edit（含 diff 不合并）
    for vm in snap.items.iter() {
        if let TuiRenderUnit::TuiCollapsedGroup(g) = vm {
            assert!(
                g.view_models.iter().all(
                    |inner| !matches!(inner, TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit")
                ),
                "含 diff 的 Edit 永不并入分组"
            );
        }
    }
}

/// 非 diff 输出（Edit 结果不含 unified diff）→ diff=None → 可正常分组。
/// [Fix flaky] 共享全局 VIEW_MODELS atom——非 serial 时与 serial 测试
/// 并行写读交错（serial_test 只互斥 serial 测试之间）。
#[test]
#[serial]
fn test_edit_plain_output_grouped_normally() {
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/x.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/x.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "e1".into(),
            output_summary: "Replaced text in src/x.rs".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e2".into(),
            tool_name: "Write".into(),
            input_summary: "src/y.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/y.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "e2".into(),
            output_summary: "Wrote 3 lines".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    // 两个无 diff 的相邻成功工具 → 分组（标题含 Edit/Write 计数）
    assert_eq!(snap.items.len(), 1, "无 diff 的相邻工具仍正常分组");
    match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(g) => {
            assert_eq!(g.count, 2);
            assert!(
                g.title.contains("Edit") || g.title.contains("Write"),
                "标题含工具名，实际 {:?}",
                g.title
            );
        }
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }
}

/// [Slice 5] 真实摘要路径：Edit 输出 `Added 2 lines to P`（真实工具形态，
/// 无 unified diff）→ 摘要 fallback 解析出 diff 块（adds=2）→ 不入组。
#[test]
#[serial]
fn test_edit_with_real_summary_diff_not_grouped() {
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "s1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/s.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/s.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
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

    // 相邻成功 Read（可合并）+ 带 diff 的 Edit → Edit 不入组
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "s2".into(),
            tool_name: "Read".into(),
            input_summary: "src/r.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/r.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
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
    assert!(has_edit_tool, "带 diff 的 Edit 保持独立展开渲染");
    let has_group = snap
        .items
        .iter()
        .any(|vm| matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_)));
    assert!(!has_group, "含 diff 的 Edit 不并入相邻 Read 组");
}

/// [Slice 5] 真实摘要同行数替换（`Replaced 1 line to P`，middleware 新形态）
/// → 解析出 diff 块（adds=dels=1）→ 含 diff 工具不合并、不分组
/// （§7：diff 工具独立展示变更摘要）。
#[test]
#[serial]
fn test_edit_same_line_replacement_with_count_not_grouped() {
    let mut state = make_fold_test_state();
    for (id, output) in [
        ("r1", "Replaced 1 line to src/a.rs"),
        ("r2", "Replaced 1 line to src/b.rs"),
    ] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Edit".into(),
                input_summary: format!("src/{}.rs", id),
                raw_input: serde_json::json!({"file_path": format!("src/{}.rs", id)}),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
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
    assert_eq!(
        snap.items.len(),
        2,
        "带 diff 计数的相邻 Edit 各自独立，不并入折叠组"
    );
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
