//! Write 工具的字段、事务、草稿与错误族（FC-WRITE-01/02、A-008 的证据面）。

#[path = "fs_support/mod.rs"]
mod support;

use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use support::{call, call_error, field, field_str, list_names, tree};

#[test]
fn test_writes_new_file_and_reports_relative_path() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("src/written.rs");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "fn a() {}\nfn b() {}\n" }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(response.text, "Wrote 2 lines src/written.rs");
    assert_eq!(tree.read("src/written.rs"), "fn a() {}\nfn b() {}\n");
    assert_eq!(field(&response, "lines"), Some(json!(2)));
    assert_eq!(field(&response, "append"), Some(json!(false)));
    assert_eq!(field(&response, "total_lines"), Some(json!(2)));
}

#[test]
fn test_single_line_message_uses_singular_label() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("one.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "only" }),
    );
    assert_eq!(response.text, "Wrote 1 line one.txt");
}

#[test]
fn test_append_preserves_existing_bytes_and_reports_total() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("notes.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "delta\n", "append": true }),
    );
    assert_eq!(
        response.text,
        "Appended 1 line to notes.txt (file total: 4 lines)"
    );
    assert_eq!(tree.read("notes.txt"), "alpha\nbeta\ngamma\ndelta\n");
}

#[test]
fn test_append_preserves_non_utf8_bytes() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    std::fs::write(tree.root.join("raw.bin"), [0xffu8, 0x00, 0x41]).expect("写入原始字节");
    let target = tree.path("raw.bin");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "Z", "append": true }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        std::fs::read(tree.root.join("raw.bin")).unwrap(),
        vec![0xffu8, 0x00, 0x41, b'Z']
    );
}

#[test]
fn test_missing_parent_directories_are_created() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("brand/new/dir/file.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "nested\n" }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(tree.read("brand/new/dir/file.txt"), "nested\n");
}

#[test]
fn test_existing_permissions_are_preserved_on_overwrite() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let path = tree.root.join("script.sh");
    std::fs::write(&path, "#!/bin/sh\n").expect("写入");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    let target = path.to_string_lossy().to_string();
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "#!/bin/sh\necho hi\n" }),
    );
    assert!(!response.is_error, "{}", response.text);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "权限位必须被保留");
}

#[test]
fn test_required_parameter_errors_match_source() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let message = call_error(&runtime, "Write", None, json!({}));
    assert_eq!(
        message,
        "The 'file_path' parameter is required for the Write tool."
    );

    let target = tree.path("x.txt");
    let message = call_error(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target }),
    );
    assert_eq!(
        message,
        "Either 'content' or 'from_draft' must be provided for the Write tool."
    );

    // 占位符 content（空串/空白/__omit__）等同未提供。
    for placeholder in ["", "   ", "__omit__"] {
        let target = tree.path("x.txt");
        let message = call_error(
            &runtime,
            "Write",
            Some(&target),
            json!({ "file_path": target, "content": placeholder }),
        );
        assert_eq!(
            message,
            "Either 'content' or 'from_draft' must be provided for the Write tool."
        );
    }
}

#[test]
fn test_content_wins_over_from_draft_and_placeholder_draft_is_ignored() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("both.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({
            "file_path": target,
            "content": "from content\n",
            "from_draft": "draft_00000000-0000-7000-0000-000000000000"
        }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(tree.read("both.txt"), "from content\n");

    let target = tree.path("placeholder.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "real\n", "from_draft": "__omit__" }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(tree.read("placeholder.txt"), "real\n");
}

#[test]
fn test_unknown_draft_degrades_with_dedicated_message() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("draft_target.txt");
    let message = call_error(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "from_draft": "draft_00000000-0000-7000-0000-000000000000" }),
    );
    assert_eq!(
        message,
        "Draft is unknown or no longer available. Retry by providing content directly."
    );
}

#[test]
fn test_io_failure_saves_draft_that_can_be_restored() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    // 目标是一个目录：rename 阶段必然失败（源实现用同类方式触发 Io）。
    let target = tree.path("empty_dir");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "recovered\n" }),
    );
    assert!(response.is_error, "写入目录必须失败: {}", response.text);
    assert!(
        response
            .text
            .starts_with("Write failed while committing the file."),
        "{}",
        response.text
    );
    assert!(
        response.text.contains("A draft was saved: draft_"),
        "{}",
        response.text
    );
    let draft_id = field_str(&response, "draft_id");
    assert!(draft_id.starts_with("draft_"), "{draft_id}");

    // 草稿恢复：换一个可写目标不合法（target 必须一致）→ 专属文案。
    let other = tree.path("other.txt");
    let message = call_error(
        &runtime,
        "Write",
        Some(&other),
        json!({ "file_path": other, "from_draft": draft_id }),
    );
    assert_eq!(
        message,
        "Draft belongs to a different file_path. Retry with the original file_path or content."
    );

    // 同一目标再次提交仍会失败（目录仍在），但错误是 IO 文案（草稿保持可用）。
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "from_draft": draft_id }),
    );
    assert!(response.is_error);
    assert_eq!(response.text, "Write failed while committing the file.");
}

#[test]
fn test_failed_commit_leaves_no_temporary_files_and_original_content_intact() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("empty_dir");
    let before = support::snapshot(&tree.root);
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "data that cannot land\n" }),
    );
    assert!(response.is_error);
    assert_eq!(
        before,
        support::snapshot(&tree.root),
        "失败提交不得留下任何变化"
    );
    assert!(
        list_names(&tree.root)
            .iter()
            .all(|name| !name.contains(".tmp.")),
        "tmp 文件必须被清理"
    );
}

#[test]
fn test_projection_sentinel_is_rejected_without_draft() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("guarded.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "... [42 字符已省略] ...\n" }),
    );
    assert!(response.is_error, "{}", response.text);
    assert!(
        response.text.contains("protected projection text"),
        "{}",
        response.text
    );
    assert!(
        !response
            .structured
            .as_object()
            .unwrap()
            .contains_key("draft_id"),
        "哨兵拒绝不落草稿: {}",
        response.structured
    );
    assert!(!tree.root.join("guarded.txt").exists(), "文件不得被创建");
}

#[test]
fn test_existing_sentinel_lines_do_not_block_edits_that_keep_them() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.root.join("kept.txt");
    std::fs::write(&target, "top\n... [7 字符已省略] ...\nbottom\n").expect("写入");
    let target_str = target.to_string_lossy().to_string();
    // 内容里保留原有哨兵行（计数不变）→ 允许。
    let response = call(
        &runtime,
        "Write",
        Some(&target_str),
        json!({
            "file_path": target_str,
            "content": "top\n... [7 字符已省略] ...\nbottom\nmore\n"
        }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "top\n... [7 字符已省略] ...\nbottom\nmore\n"
    );
}

#[test]
fn test_writing_the_workspace_root_itself_fails_closed() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let root = tree.root_str();
    let response = call(
        &runtime,
        "Write",
        Some(&root),
        json!({ "file_path": root, "content": "nope\n" }),
    );
    assert!(response.is_error, "{}", response.text);
    assert!(
        response
            .text
            .starts_with("Write failed while committing the file."),
        "{}",
        response.text
    );
}

#[test]
fn test_structured_fields_cover_tool_ok_and_error_shapes() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("structured.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "x\n" }),
    );
    for key in [
        "tool",
        "ok",
        "truncated",
        "path",
        "lines",
        "append",
        "total_lines",
    ] {
        assert!(
            response.structured.get(key).is_some(),
            "结构化字段缺失 {key}: {}",
            response.structured
        );
    }
    assert_eq!(field(&response, "ok"), Some(json!(true)));

    let response = call(&runtime, "Write", None, json!({}));
    assert_eq!(field(&response, "ok"), Some(json!(false)));
    assert_eq!(field(&response, "tool"), Some(json!("Write")));
}

#[test]
fn test_append_to_missing_file_creates_it_with_total_lines() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("brand_new.txt");
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": "a\nb\n", "append": true }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        response.text,
        "Appended 2 lines to brand_new.txt (file total: 2 lines)"
    );
    assert_eq!(tree.read("brand_new.txt"), "a\nb\n");
}

#[test]
fn test_large_content_is_written_without_truncation() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("big.txt");
    let content = "0123456789\n".repeat(5_000);
    let response = call(
        &runtime,
        "Write",
        Some(&target),
        json!({ "file_path": target, "content": content }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(response.text, "Wrote 5000 lines big.txt");
    assert_eq!(tree.read("big.txt").len(), content.len());
}
