//! Read 工具的字段、分支与错误族（FC-READ-01/02、A-007 的证据面）。

#[path = "fs_support/mod.rs"]
mod support;

use serde_json::json;

use support::{call, call_error, field, field_str, path_args, sparse_file, tree};

fn write_fixture(tree: &support::Tree, relative: &str, content: &str) {
    let target = tree.root.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).expect("父目录");
    }
    std::fs::write(&target, content).expect("写入夹具");
}

#[test]
fn test_numbers_lines_and_reports_structured_facts() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("notes.txt")),
        path_args(&tree.path("notes.txt")),
    );
    assert!(!response.is_error);
    // 行号右对齐 6 位 + TAB；末尾换行符产生一个空行（源实现 split('\n') 语义）。
    assert_eq!(
        response.text,
        "     1\talpha\n     2\tbeta\n     3\tgamma\n     4\t"
    );
    assert_eq!(field(&response, "start_line"), Some(json!(1)));
    assert_eq!(field(&response, "total_lines"), Some(json!(4)));
    assert_eq!(field(&response, "truncated"), Some(json!(false)));
    assert_eq!(field_str(&response, "tool"), "Read");
}

#[test]
fn test_offset_and_limit_slice_like_source() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("notes.txt")),
        json!({ "file_path": tree.path("notes.txt"), "offset": 2, "limit": 2 }),
    );
    assert_eq!(response.text, "     2\tbeta\n     3\tgamma");
    assert_eq!(field(&response, "start_line"), Some(json!(2)));
    assert_eq!(field(&response, "end_line"), Some(json!(3)));

    // offset 指向最后一行（空行）也合法
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("notes.txt")),
        json!({ "file_path": tree.path("notes.txt"), "offset": 4 }),
    );
    assert_eq!(response.text, "     4\t");
}

#[test]
fn test_offset_beyond_file_length_is_an_error_with_range() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let message = call_error(
        &runtime,
        "Read",
        Some(&tree.path("notes.txt")),
        json!({ "file_path": tree.path("notes.txt"), "offset": 5 }),
    );
    assert_eq!(
        message,
        "Error: offset 5 exceeds file length (4 lines). Valid offsets are 1..=4; omit offset to read from the beginning. Do not guess another offset or use offset to probe the file end."
    );
}

#[test]
fn test_invalid_offset_and_limit_types_are_rejected() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let file = tree.path("notes.txt");
    for (arguments, expected) in [
        (
            json!({ "file_path": file, "offset": 0 }),
            "Error: 'offset' must be a positive integer (1-based line number), got 0",
        ),
        (
            json!({ "file_path": file, "offset": 1.5 }),
            "Error: 'offset' must be a positive integer (1-based line number), got 1.5",
        ),
        (
            json!({ "file_path": file, "limit": -3 }),
            "Error: 'limit' must be a positive integer (1-based line number), got -3",
        ),
        (
            json!({ "file_path": file, "limit": "many" }),
            "Error: 'limit' must be a positive integer, got \"many\"",
        ),
    ] {
        let message = call_error(&runtime, "Read", Some(&file), arguments);
        assert_eq!(message, expected);
    }
}

#[test]
fn test_missing_file_path_is_reported_with_source_message() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let message = call_error(&runtime, "Read", None, json!({}));
    assert_eq!(
        message,
        "The 'file_path' parameter is required for the Read tool. Provide the absolute path to the file."
    );
}

#[test]
fn test_missing_file_reports_requested_path_verbatim() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let missing = tree.path("src/does_not_exist.rs");
    let message = call_error(&runtime, "Read", Some(&missing), path_args(&missing));
    assert_eq!(message, format!("Error: File not found at {missing}"));
}

#[test]
fn test_binary_extensions_are_detected_before_existence_check() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    write_fixture(&tree, "logo.png", "not really a png");
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("logo.png")),
        path_args(&tree.path("logo.png")),
    );
    assert!(response.text.starts_with("[BINARY FILE DETECTED]"));
    assert!(response.text.contains("File type: .png"));
    assert!(response.text.contains(&tree.path("logo.png")));

    // 扩展名判定先于存在性：不存在的 .png 也返回二进制分支（源实现同序）。
    let missing = tree.path("ghost.png");
    let response = call(&runtime, "Read", Some(&missing), path_args(&missing));
    assert!(!response.is_error, "{}", response.text);
    assert!(response.text.starts_with("[BINARY FILE DETECTED]"));
}

#[test]
fn test_pdf_branches_match_source() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    write_fixture(&tree, "doc.pdf", "%PDF-1.4 fake");

    let with_pages = call(
        &runtime,
        "Read",
        Some(&tree.path("doc.pdf")),
        json!({ "file_path": tree.path("doc.pdf"), "pages": "1-5" }),
    );
    assert!(with_pages
        .text
        .starts_with("[PDF READING NOT YET SUPPORTED]"));
    assert!(with_pages
        .text
        .contains("Use the Bash tool with a PDF reader command"));

    let without_pages = call(
        &runtime,
        "Read",
        Some(&tree.path("doc.pdf")),
        path_args(&tree.path("doc.pdf")),
    );
    assert!(without_pages.text.starts_with("[BINARY FILE DETECTED]"));
}

#[test]
fn test_empty_file_and_directory_branches() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    write_fixture(&tree, "blank.txt", "");
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("blank.txt")),
        path_args(&tree.path("blank.txt")),
    );
    assert!(
        response.text.starts_with("[EMPTY FILE]"),
        "{}",
        response.text
    );
    assert!(response.text.contains("The file is empty (0 bytes)."));

    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("src")),
        path_args(&tree.path("src")),
    );
    assert!(
        response.text.starts_with("[DIRECTORY DETECTED]"),
        "{}",
        response.text
    );
    assert!(response.text.contains("folder_operations"));
    assert!(
        response.text.contains("Total: 1 directories, 2 files"),
        "目录 listing 统计: {}",
        response.text
    );
    assert!(
        response.text.contains("  \u{1F4C1} nested/ ("),
        "目录条目格式: {}",
        response.text
    );
    assert!(
        response.text.contains("  \u{1F4C4} lib.rs ("),
        "文件条目格式: {}",
        response.text
    );
}

#[test]
fn test_oversized_file_is_rejected_even_with_offset() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    sparse_file(&tree.root.join("huge.txt"), 33 * 1024 * 1024);
    let message = call_error(
        &runtime,
        "Read",
        Some(&tree.path("huge.txt")),
        json!({ "file_path": tree.path("huge.txt"), "offset": 2, "limit": 5 }),
    );
    assert!(
        message.starts_with("Error: File too large (34603008 bytes, max 33554432 bytes)."),
        "{message}"
    );
    assert!(message.contains("offset/limit cannot bypass the file-size limit"));
}

#[test]
fn test_non_utf8_file_reports_std_error_text() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    std::fs::write(tree.root.join("blob.txt"), [0xffu8, 0xfe, 0x00]).expect("写入非 UTF-8");
    let message = call_error(
        &runtime,
        "Read",
        Some(&tree.path("blob.txt")),
        path_args(&tree.path("blob.txt")),
    );
    assert_eq!(message, "stream did not contain valid UTF-8");
}

#[test]
fn test_long_lines_get_prepended_truncation_marker() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let long_line = "a".repeat(70_000);
    write_fixture(&tree, "long.txt", &format!("{long_line}\nshort\n"));
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("long.txt")),
        path_args(&tree.path("long.txt")),
    );
    assert!(!response.is_error, "{}", response.text);
    assert!(
        response.text.contains("     1\t[LINE TRUNCATED: 70000 characters total; retained first 65536 characters before output-level truncation] "),
        "行号前缀 + 截断标记: {}",
        &response.text[..160.min(response.text.len())]
    );
    assert_eq!(
        response.text.lines().count(),
        2,
        "截断标记必须前置在内容之前"
    );
    assert_eq!(field(&response, "truncated"), Some(json!(true)));
}

#[test]
fn test_output_over_byte_limit_truncates_at_line_boundary_and_continues() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let mut content = String::new();
    for index in 1..=80 {
        content.push_str(&format!("{index:0>100}\n"));
    }
    write_fixture(&tree, "wide.txt", &content);
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("wide.txt")),
        path_args(&tree.path("wide.txt")),
    );
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(field(&response, "truncated"), Some(json!(true)));
    let footer = response
        .text
        .lines()
        .last()
        .expect("截断提示行")
        .to_string();
    assert!(footer.starts_with("[Output truncated:"), "{footer}");
    assert!(footer.contains("showing lines 1..="), "{footer}");
    assert!(footer.contains("continue reading with offset="), "{footer}");

    // 按提示继续读取：能拿到剩余内容。
    let next_offset: u64 = footer
        .split("offset=")
        .nth(1)
        .and_then(|tail| tail.trim_end_matches(']').parse().ok())
        .expect("解析续读 offset");
    let continuation = call(
        &runtime,
        "Read",
        Some(&tree.path("wide.txt")),
        json!({ "file_path": tree.path("wide.txt"), "offset": next_offset }),
    );
    assert!(!continuation.is_error, "{}", continuation.text);
    assert!(
        continuation.text.contains("0080"),
        "续读必须拿到剩余行: {}",
        &continuation.text[..80.min(continuation.text.len())]
    );
    assert!(
        continuation
            .text
            .lines()
            .next()
            .unwrap()
            .trim_start()
            .starts_with("47"),
        "{}",
        continuation.text
    );
}

#[test]
fn test_single_line_over_budget_falls_back_to_byte_truncation() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let huge_line = "x".repeat(6_000);
    write_fixture(&tree, "one_line.txt", &format!("{huge_line}\n"));
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("one_line.txt")),
        path_args(&tree.path("one_line.txt")),
    );
    assert!(!response.is_error, "{}", response.text);
    assert!(
        response
            .text
            .contains("line 2 exceeds the output limit; use offset=2 to read the rest of the file"),
        "{}",
        &response.text[response.text.len().saturating_sub(200)..]
    );
}

#[test]
fn test_relative_paths_resolve_against_the_workspace_root() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    // 相对路径按工作区根解析；根内绝对路径（规范形态）同样被接受。
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("notes.txt")),
        json!({ "file_path": "notes.txt" }),
    );
    assert!(!response.is_error, "{}", response.text);
    assert!(response.text.contains("alpha"));
}

#[test]
fn test_reads_through_in_root_symlink_are_allowed_but_escapes_are_not() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let allowed = call(
        &runtime,
        "Read",
        Some(&tree.path("link_to_file")),
        path_args(&tree.path("link_to_file")),
    );
    assert!(allowed.text.contains("pub fn add"), "{}", allowed.text);

    let denied = call_error(
        &runtime,
        "Read",
        Some(&tree.path("link_outside_file")),
        path_args(&tree.path("link_outside_file")),
    );
    assert!(!denied.contains("OUTSIDE-SECRET-CONTENT"), "{denied}");
}

#[test]
fn test_crlf_line_endings_are_preserved_in_numbered_output() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    write_fixture(&tree, "crlf.txt", "first\r\nsecond\r\n");
    let response = call(
        &runtime,
        "Read",
        Some(&tree.path("crlf.txt")),
        path_args(&tree.path("crlf.txt")),
    );
    assert!(!response.is_error, "{}", response.text);
    // 源实现按 '\n' 切分：行内保留 '\r'，末尾空行照旧计一行。
    assert_eq!(response.text, "     1\tfirst\r\n     2\tsecond\r\n     3\t");
}

#[test]
fn test_reading_a_file_with_spaces_and_unicode_names_works() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    write_fixture(&tree, "docs/带 空格 的文件.md", "内容\n");
    let path = tree.path("docs/带 空格 的文件.md");
    let response = call(&runtime, "Read", Some(&path), path_args(&path));
    assert!(!response.is_error, "{}", response.text);
    assert!(response.text.contains("内容"), "{}", response.text);
}
