//! Edit 工具：唯一性、读前写、模糊提示与事务（FC-EDIT-01/02 的证据面）。

#[path = "fs_support/mod.rs"]
mod support;

use serde_json::json;

use local_mcp_server::wire::ToolResponse;

use support::{call, call_error, field, tree};

fn write(tree: &support::Tree, relative: &str, content: &str) {
    std::fs::write(tree.root.join(relative), content).expect("写入夹具");
}

fn edit(tree: &support::Tree, relative: &str, old: &str, new: &str) -> ToolResponse {
    let runtime = tree.runtime();
    let target = tree.path(relative);
    call(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": old, "new_string": new }),
    )
}

#[test]
fn test_replaces_single_occurrence_and_reports_line_delta() {
    let tree = tree("basic");
    let response = edit(&tree, "edit_me.txt", "two", "TWO");
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(response.text, "Replaced 1 line to edit_me.txt");
    assert_eq!(tree.read("edit_me.txt"), "one\nTWO\nthree\n");
    assert_eq!(field(&response, "occurrences"), Some(json!(1)));
    assert_eq!(field(&response, "line_delta"), Some(json!(0)));
}

#[test]
fn test_multi_line_edits_report_added_and_removed_lines() {
    let tree = tree("basic");
    let removed = edit(&tree, "edit_me.txt", "one\ntwo\n", "one\n");
    assert_eq!(removed.text, "Removed 1 line to edit_me.txt");
    assert_eq!(tree.read("edit_me.txt"), "one\nthree\n");

    let added = edit(&tree, "edit_me.txt", "one\n", "one\ninserted\n");
    assert_eq!(added.text, "Added 1 line to edit_me.txt");
    assert_eq!(tree.read("edit_me.txt"), "one\ninserted\nthree\n");
}

#[test]
fn test_replace_all_rewrites_every_occurrence_with_count() {
    let tree = tree("basic");
    write(&tree, "repeat.txt", "foo\nfoo\nfoo\n");
    let runtime = tree.runtime();
    let target = tree.path("repeat.txt");
    let response = call(
        &runtime,
        "Edit",
        Some(&target),
        json!({
            "file_path": target,
            "old_string": "foo",
            "new_string": "bar",
            "replace_all": true
        }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        response.text,
        "Replaced 1 line to repeat.txt (replaced 3 occurrences)"
    );
    assert_eq!(tree.read("repeat.txt"), "bar\nbar\nbar\n");
    assert_eq!(field(&response, "replace_all"), Some(json!(true)));
}

#[test]
fn test_duplicate_old_string_is_rejected_with_locations() {
    let tree = tree("basic");
    write(&tree, "dup.txt", "same\nother\nsame\nsame\n");
    let runtime = tree.runtime();
    let target = tree.path("dup.txt");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "same", "new_string": "x" }),
    );
    assert!(
        message.starts_with("Error: old_string is not unique in "),
        "{message}"
    );
    assert!(message.contains("(found 3 occurrences)."), "{message}");
    assert!(
        message.contains("Match locations: line 1, line 3, line 4."),
        "{message}"
    );
    assert!(message.contains("set replace_all=true"), "{message}");
    // 未做任何修改
    assert_eq!(tree.read("dup.txt"), "same\nother\nsame\nsame\n");
}

#[test]
fn test_not_found_hint_strategy_one_reports_matched_prefix_lines() {
    let tree = tree("basic");
    write(&tree, "hint1.txt", "l1\nl2\nl3\nl4\nl5\nl6\ntail\n");
    let runtime = tree.runtime();
    let target = tree.path("hint1.txt");
    let old = "l2\nl3\nl4\nl5\nl6\nCHANGED";
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": old, "new_string": "x" }),
    );
    assert!(
        message.contains("Error: old_string not found in "),
        "{message}"
    );
    assert!(
        message.contains(
            "old_string's first 5 lines matched lines 2-6, but the full string did not match."
        ),
        "{message}"
    );
}

#[test]
fn test_not_found_hint_strategy_two_reports_closest_window() {
    let tree = tree("basic");
    write(&tree, "hint2.txt", "x\n  beta\n  gamma\n");
    let runtime = tree.runtime();
    let target = tree.path("hint2.txt");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "beta\ngamma", "new_string": "x" }),
    );
    assert!(
        message.contains("Closest match at lines 2-3 (0 of 2 lines differ)."),
        "{message}"
    );
}

#[test]
fn test_not_found_hint_falls_back_without_fuzzy_match() {
    let tree = tree("basic");
    write(&tree, "hint3.txt", "nothing here\n");
    let runtime = tree.runtime();
    let target = tree.path("hint3.txt");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({
            "file_path": target,
            "old_string": "completely different\nmulti\nline",
            "new_string": "x"
        }),
    );
    assert!(
        message.ends_with("Please Read this file to get the latest content before retrying."),
        "{message}"
    );
}

#[test]
fn test_empty_old_string_is_rejected() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("edit_me.txt");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "", "new_string": "x" }),
    );
    assert_eq!(message, "Error: old_string cannot be empty");
}

#[test]
fn test_required_parameters_report_source_messages() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("edit_me.txt");
    let cases = [
        (
            json!({}),
            "The 'file_path' parameter is required for the Edit tool.",
        ),
        (
            json!({ "file_path": target }),
            "The 'old_string' parameter is required for the Edit tool.",
        ),
        (
            json!({ "file_path": target, "old_string": "one" }),
            "The 'new_string' parameter is required for the Edit tool.",
        ),
    ];
    for (arguments, expected) in cases {
        let message = call_error(&runtime, "Edit", Some(&target), arguments);
        assert_eq!(message, expected);
    }
}

#[test]
fn test_missing_file_reports_file_not_found() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("src/absent.rs");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "a", "new_string": "b" }),
    );
    assert_eq!(message, "Error: File not found");
}

#[test]
fn test_non_utf8_file_reports_read_failure() {
    let tree = tree("basic");
    std::fs::write(tree.root.join("raw.txt"), [0xffu8, 0xfe]).expect("写入非 UTF-8");
    let runtime = tree.runtime();
    let target = tree.path("raw.txt");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "a", "new_string": "b" }),
    );
    assert_eq!(message, "Edit failed while reading the file.");
}

#[test]
fn test_editing_a_directory_reports_read_failure() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("empty_dir");
    let message = call_error(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "a", "new_string": "b" }),
    );
    assert_eq!(message, "Edit failed while reading the file.");
}

#[test]
fn test_sentinel_introduction_is_rejected_and_file_untouched() {
    let tree = tree("basic");
    let before = tree.read("edit_me.txt");
    let runtime = tree.runtime();
    let target = tree.path("edit_me.txt");
    let response = call(
        &runtime,
        "Edit",
        Some(&target),
        json!({
            "file_path": target,
            "old_string": "two",
            "new_string": "two\n... [9 字符已省略] ..."
        }),
    );
    assert!(response.is_error, "{}", response.text);
    assert!(
        response.text.contains("protected projection text"),
        "{}",
        response.text
    );
    assert_eq!(tree.read("edit_me.txt"), before);
}

#[test]
fn test_editing_through_in_root_symlink_updates_target() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let target = tree.path("link_to_file");
    let response = call(
        &runtime,
        "Edit",
        Some(&target),
        json!({ "file_path": target, "old_string": "pub fn add", "new_string": "pub fn sum" }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert!(tree.read("src/lib.rs").contains("pub fn sum"));
}

#[test]
fn test_concurrent_edits_to_same_target_are_serialized() {
    let tree = tree("basic");
    write(&tree, "counter.txt", "start\n");
    let runtime = std::sync::Arc::new(tree.runtime());
    let target = tree.path("counter.txt");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let runtime = std::sync::Arc::clone(&runtime);
        let target = target.clone();
        handles.push(std::thread::spawn(move || {
            // 同一目标的并发 Edit（内容替换为自身）必须被 per-target 锁串行化：
            // 任一线程都不得读到半成品或把文件写坏。
            support::call(
                &runtime,
                "Edit",
                Some(&target),
                json!({
                    "file_path": target,
                    "old_string": "start",
                    "new_string": "start",
                    "replace_all": true
                }),
            )
        }));
    }
    for handle in handles {
        let response = handle.join().expect("线程不得 panic");
        assert!(
            !response.is_error || response.text.contains("not found"),
            "并发编辑只允许成功或未命中: {}",
            response.text
        );
    }
    assert_eq!(tree.read("counter.txt"), "start\n", "文件必须保持一致");
    assert!(
        std::fs::read_dir(&tree.root)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp.")),
        "不得残留 tmp 文件"
    );
}
