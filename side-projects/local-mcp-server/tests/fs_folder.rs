//! `folder_operations`：四个操作、截断落盘、tree 格式与错误族（FC-FOLDER-01/02 的证据面）。

#[path = "fs_support/mod.rs"]
mod support;

use serde_json::json;

use local_mcp_server::wire::ToolResponse;

use support::{call, call_error, field, field_str, folder_args, tree};

fn folder(
    tree: &support::Tree,
    operation: &str,
    path: &str,
    extra: serde_json::Value,
) -> ToolResponse {
    let runtime = tree.runtime();
    let mut arguments = folder_args(operation, path);
    if let Some(object) = extra.as_object() {
        for (key, value) in object {
            arguments[key] = value.clone();
        }
    }
    call(&runtime, "folder_operations", Some(path), arguments)
}

#[test]
fn test_create_recursive_and_single_level() {
    let tree = tree("basic");
    let deep = tree.path("a/b/c");
    let response = folder(&tree, "create", &deep, json!({}));
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(
        response.text,
        format!("\u{2713} Folder created successfully at: {deep}")
    );
    assert!(tree.root.join("a/b/c").is_dir());

    let single = tree.path("a/b/c/d");
    let response = folder(&tree, "create", &single, json!({ "recursive": false }));
    assert!(!response.is_error, "{}", response.text);
    assert!(tree.root.join("a/b/c/d").is_dir());

    // 非递归创建缺少父目录的路径 → 源实现原样抛出 io 错误。
    let missing_parent = tree.path("no/dir");
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&missing_parent),
        json!({ "operation": "create", "folder_path": missing_parent, "recursive": false }),
    );
    assert_eq!(
        message,
        std::io::Error::from_raw_os_error(2).to_string(),
        "源实现把底层 io 错误原样外发"
    );

    // 递归创建已存在的目录是幂等的（create_dir_all 语义）。
    let response = folder(&tree, "create", &deep, json!({}));
    assert!(!response.is_error, "{}", response.text);

    // 非递归创建已存在的目录 → EEXIST 文案（源实现原样外发 io 错误）。
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&deep),
        json!({ "operation": "create", "folder_path": deep, "recursive": false }),
    );
    assert_eq!(message, std::io::Error::from_raw_os_error(17).to_string());
}

#[test]
fn test_exists_reports_type_without_failing_on_files() {
    let tree = tree("basic");
    let directory = folder(&tree, "exists", &tree.path("src"), json!({}));
    assert!(directory.text.contains("\u{2713} Folder exists at: "));
    assert!(directory.text.contains("  Type: Directory"));
    assert_eq!(field(&directory, "kind"), Some(json!("Directory")));

    let file = folder(&tree, "exists", &tree.path("notes.txt"), json!({}));
    assert!(!file.is_error, "文件不得被当成错误: {}", file.text);
    assert!(file.text.contains("  Type: File"));
    assert_eq!(field(&file, "kind"), Some(json!("File")));

    let missing = folder(&tree, "exists", &tree.path("ghost"), json!({}));
    assert!(!missing.is_error);
    assert!(missing
        .text
        .starts_with("\u{2717} Folder does not exist at: "));
    assert_eq!(field(&missing, "exists"), Some(json!(false)));
}

#[test]
fn test_list_reports_entries_with_sizes_and_dates() {
    let tree = tree("basic");
    let listing = folder(&tree, "list", &tree.path("src"), json!({}));
    assert!(!listing.is_error, "{}", listing.text);
    assert!(listing
        .text
        .starts_with(&format!("\u{1F4C1} {}", tree.path("src"))));
    assert!(listing.text.contains("Directories:\n  \u{1F4C1} nested/ ("));
    assert!(listing.text.contains("Files:\n  \u{1F4C4} lib.rs ("));
    assert!(
        listing.text.contains(" bytes, 20"),
        "日期格式: {}",
        listing.text
    );
    assert!(listing.text.ends_with("Total: 1 directories, 2 files"));
    assert_eq!(field(&listing, "entries"), Some(json!(3)));
    assert_eq!(field(&listing, "truncated"), Some(json!(false)));
}

#[test]
fn test_list_errors_for_missing_and_non_directory_paths() {
    let tree = tree("basic");
    let missing = tree.path("ghost");
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&missing),
        folder_args("list", &missing),
    );
    assert_eq!(message, format!("Folder not found: {missing}"));

    let file = tree.path("notes.txt");
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&file),
        folder_args("list", &file),
    );
    assert_eq!(message, format!("Path exists but is not a folder: {file}"));
}

#[test]
fn test_list_truncates_and_persists_full_listing() {
    let tree = tree("basic");
    let bulk = tree.root.join("many");
    std::fs::create_dir_all(&bulk).expect("创建目录");
    for index in 0..300 {
        std::fs::create_dir_all(bulk.join(format!("d{index:03}"))).expect("创建子目录");
    }
    for index in 0..300 {
        std::fs::write(bulk.join(format!("f{index:03}.txt")), "x\n").expect("写入文件");
    }
    let listing = folder(&tree, "list", &bulk.to_string_lossy(), json!({}));
    assert!(!listing.is_error, "{}", listing.text);
    assert_eq!(field(&listing, "truncated"), Some(json!(true)));
    assert_eq!(field(&listing, "entries"), Some(json!(600)));
    assert!(
        listing
            .text
            .contains("[Output truncated: 600 total entries, showing first 500]"),
        "{}",
        listing.text
    );
    // 公平分配：目录 250 条 + 文件 250 条。
    assert_eq!(listing.text.matches("\u{1F4C1} d").count(), 250);
    assert_eq!(listing.text.matches("\u{1F4C4} f").count(), 250);
    assert!(listing.text.ends_with("Total: 300 directories, 300 files"));

    let persisted = field_str(&listing, "persisted_path");
    let full = std::fs::read_to_string(&persisted).expect("读取落盘全量");
    assert_eq!(full.lines().count(), 601, "落盘内容 = 600 条 + 统计行");
}

#[test]
fn test_deep_scan_renders_tree_and_skips_blacklisted_dirs() {
    let tree = tree("basic");
    let scan = folder(
        &tree,
        "deep_scan",
        &tree.root_str(),
        json!({ "max_depth": 3 }),
    );
    assert!(!scan.is_error, "{}", scan.text);
    assert!(scan
        .text
        .starts_with(&format!("\u{1F4C1} {}\n\n", tree.root_str())));
    // 源实现的 is_last 规则：下一个条目的父目录与当前不同即标记为「最后一项」，
    // 因此子树的第一个条目也会带 `└──` 连接符（逐字迁移该行为，不做美化）。
    assert!(
        scan.text
            .contains("\u{2514}\u{2500}\u{2500} \u{1F4C1} src/ ("),
        "{}",
        scan.text
    );
    assert!(
        scan.text
            .contains("    \u{251C}\u{2500}\u{2500} \u{1F4C4} lib.rs ("),
        "同层非末项用 ├── : {}",
        scan.text
    );
    assert!(
        scan.text
            .contains("    \u{251C}\u{2500}\u{2500} \u{1F4C4} main.rs ("),
        "{}",
        scan.text
    );
    assert!(
        scan.text
            .contains("    \u{2514}\u{2500}\u{2500} \u{1F4C1} nested/ ("),
        "同层末项用 └── : {}",
        scan.text
    );
    assert!(
        scan.text
            .contains("        \u{2514}\u{2500}\u{2500} \u{1F4C4} deep.txt ("),
        "缩进必须体现深度: {}",
        scan.text
    );
    assert!(scan.text.contains(" bytes, 20"), "{}", scan.text);
    for skipped in ["node_modules", ".git", "target"] {
        assert!(
            !scan.text.contains(skipped),
            "skip dir {skipped}: {}",
            scan.text
        );
    }
    assert!(scan.text.ends_with("Total: 16 entries"), "{}", scan.text);
    assert_eq!(field_str(&scan, "operation"), "deep_scan");
}

#[test]
fn test_deep_scan_depth_is_clamped_to_documented_range() {
    let tree = tree("basic");
    // max_depth=1：只有根的直接子项。
    let shallow = folder(
        &tree,
        "deep_scan",
        &tree.root_str(),
        json!({ "max_depth": 1 }),
    );
    assert!(!shallow.text.contains("deep.txt"), "{}", shallow.text);
    assert!(shallow.text.contains("\u{1F4C1} src/"), "{}", shallow.text);

    // 越界值被 clamp 到 [1,10]（不报错）。
    let clamped_low = folder(
        &tree,
        "deep_scan",
        &tree.root_str(),
        json!({ "max_depth": 0 }),
    );
    assert!(!clamped_low.is_error, "{}", clamped_low.text);
    assert_eq!(field(&clamped_low, "max_depth"), Some(json!(1)));
    let clamped_high = folder(
        &tree,
        "deep_scan",
        &tree.root_str(),
        json!({ "max_depth": 99 }),
    );
    assert_eq!(field(&clamped_high, "max_depth"), Some(json!(10)));

    // 非法类型显式报错（源实现 parse_optional_u64）。
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&tree.root_str()),
        json!({
            "operation": "deep_scan",
            "folder_path": tree.root_str(),
            "max_depth": "deep"
        }),
    );
    assert_eq!(
        message,
        "Error: 'max_depth' must be a non-negative integer, got \"deep\""
    );
}

#[test]
fn test_deep_scan_rejects_missing_and_non_directory_paths() {
    let tree = tree("basic");
    let missing = tree.path("ghost");
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&missing),
        folder_args("deep_scan", &missing),
    );
    assert_eq!(message, format!("Folder not found: {missing}"));

    let file = tree.path("notes.txt");
    let message = call_error(
        &tree.runtime(),
        "folder_operations",
        Some(&file),
        folder_args("deep_scan", &file),
    );
    assert_eq!(message, format!("Path exists but is not a folder: {file}"));
}

#[test]
fn test_unknown_operation_and_missing_parameters_are_reported() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let message = call_error(
        &runtime,
        "folder_operations",
        Some(&tree.root_str()),
        json!({ "operation": "frobnicate", "folder_path": tree.root_str() }),
    );
    assert_eq!(message, "Unknown operation: frobnicate");

    let message = call_error(
        &runtime,
        "folder_operations",
        None,
        json!({ "folder_path": tree.root_str() }),
    );
    assert_eq!(message, "Missing operation parameter");

    let message = call_error(
        &runtime,
        "folder_operations",
        None,
        json!({ "operation": "list" }),
    );
    assert_eq!(message, "Missing folder_path parameter");
}

#[test]
fn test_deep_scan_truncates_long_listings_and_persists() {
    let tree = tree("basic");
    let bulk = tree.root.join("deep");
    std::fs::create_dir_all(&bulk).expect("创建目录");
    for index in 0..520 {
        std::fs::write(bulk.join(format!("f{index:04}.txt")), "x\n").expect("写入");
    }
    let scan = folder(
        &tree,
        "deep_scan",
        &bulk.to_string_lossy(),
        json!({ "max_depth": 2 }),
    );
    assert!(!scan.is_error, "{}", scan.text);
    assert_eq!(field(&scan, "truncated"), Some(json!(true)));
    assert_eq!(field(&scan, "entries"), Some(json!(520)));
    assert!(
        scan.text
            .contains("[Output truncated: 520 total entries, showing first 500]"),
        "{}",
        scan.text
    );
    assert!(scan.text.ends_with("Total: 520 entries"), "{}", scan.text);
    let persisted = field_str(&scan, "persisted_path");
    assert!(std::path::Path::new(&persisted).exists(), "{persisted}");
}
