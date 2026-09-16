//! Glob 工具：匹配语义、排序、跳过目录、截断与落盘（FC-GLOB-01/02 的证据面）。

#[path = "fs_support/mod.rs"]
mod support;

use std::time::{Duration, SystemTime};

use serde_json::json;

use local_mcp_server::wire::ToolResponse;

use support::{call, call_error, field, field_str, tree};

fn glob(tree: &support::Tree, pattern: &str, path: Option<&str>) -> ToolResponse {
    let runtime = tree.runtime();
    // FsCall.path 是 broker 翻译后的搜索根：跟随 `path` 参数，缺省即工作区根。
    let call_path = path.map(str::to_string).unwrap_or_else(|| tree.root_str());
    let arguments = match path {
        Some(path) => json!({ "pattern": pattern, "path": path }),
        None => json!({ "pattern": pattern }),
    };
    call(&runtime, "Glob", Some(&call_path), arguments)
}

#[test]
fn test_matches_recursively_and_reports_absolute_paths() {
    let tree = tree("basic");
    let response = glob(&tree, "**/*.rs", Some(&tree.root_str()));
    assert!(!response.is_error, "{}", response.text);
    let lines: Vec<&str> = response.text.lines().collect();
    assert_eq!(lines.len(), 2, "{}", response.text);
    assert!(lines.iter().all(|line| line.starts_with(&tree.root_str())));
    assert!(response.text.contains("/src/lib.rs"));
    assert!(response.text.contains("/src/main.rs"));
    assert_eq!(field(&response, "count"), Some(json!(2)));
    assert_eq!(field(&response, "early_stopped"), Some(json!(false)));
}

#[test]
fn test_matches_relative_to_the_search_root() {
    let tree = tree("basic");
    // path=src 时，pattern 以 src 为基准；`**/*.rs` 与 `*.rs` 都应命中同一批文件。
    for pattern in ["**/*.rs", "*.rs"] {
        let response = glob(&tree, pattern, Some(&tree.path("src")));
        assert!(!response.is_error, "{}", response.text);
        assert!(
            response.text.contains("/src/lib.rs") && response.text.contains("/src/main.rs"),
            "pattern {pattern}: {}",
            response.text
        );
        assert!(
            !response.text.contains("/edit_me.txt"),
            "pattern {pattern} 不得越出搜索根: {}",
            response.text
        );
    }
}

#[test]
fn test_results_are_sorted_by_mtime_descending() {
    let tree = tree("basic");
    let scope = tree.root.join("mtimes");
    std::fs::create_dir_all(&scope).expect("创建目录");
    let old = scope.join("aaa_old.txt");
    let new = scope.join("zzz_new.txt");
    std::fs::write(&old, "old\n").expect("写入");
    std::fs::write(&new, "new\n").expect("写入");
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(base)
        .expect("设置 mtime");
    std::fs::File::options()
        .write(true)
        .open(&new)
        .unwrap()
        .set_modified(base + Duration::from_secs(600))
        .expect("设置 mtime");

    let response = glob(&tree, "*.txt", Some(&scope.to_string_lossy()));
    let lines: Vec<&str> = response.text.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "只应命中 scope 下的两个文件: {}",
        response.text
    );
    assert!(
        lines[0].ends_with("zzz_new.txt") && lines[1].ends_with("aaa_old.txt"),
        "mtime 降序：最新在前，实际 {}",
        response.text
    );
}

#[test]
fn test_skip_directories_are_pruned() {
    let tree = tree("basic");
    let response = glob(&tree, "**/*", Some(&tree.root_str()));
    for skipped in ["node_modules", "/.git/", "/target/"] {
        assert!(
            !response.text.contains(skipped),
            "被跳目录 {skipped} 不得出现在结果里: {}",
            response.text
        );
    }
    assert!(response.text.contains("/src/lib.rs"), "{}", response.text);
}

#[test]
fn test_literal_prefix_narrowing_keeps_results_equivalent() {
    let tree = tree("basic");
    let narrowed = glob(&tree, "src/**/*.rs", Some(&tree.root_str()));
    let wide = glob(&tree, "**/*.rs", Some(&tree.root_str()));
    // 收窄只影响遍历范围，不影响命中集合（root 内 src 下没有同名文件）。
    assert!(narrowed.text.contains("/src/lib.rs"));
    assert!(narrowed.text.contains("/src/main.rs"));
    assert_eq!(field(&narrowed, "count"), field(&wide, "count"));
}

#[test]
fn test_symlinked_directories_are_not_descended() {
    let tree = tree("basic");
    // link_to_dir -> src；结果里每个文件只应出现一次。
    let response = glob(&tree, "**/*.rs", Some(&tree.root_str()));
    assert_eq!(
        response.text.matches("/src/lib.rs").count(),
        1,
        "{}",
        response.text
    );
}

#[test]
fn test_no_matches_is_not_an_error() {
    let tree = tree("basic");
    let response = glob(&tree, "**/*.does-not-exist", Some(&tree.root_str()));
    assert!(!response.is_error);
    assert_eq!(response.text, "No files found.");
    assert_eq!(field(&response, "count"), Some(json!(0)));
}

#[test]
fn test_invalid_pattern_is_rejected_with_source_message() {
    let tree = tree("basic");
    let message = call_error(
        &tree.runtime(),
        "Glob",
        Some(&tree.root_str()),
        json!({ "pattern": "src/**[", "path": tree.root_str() }),
    );
    assert!(
        message.starts_with("Error: Pattern syntax error in \"src/**[\": "),
        "{message}"
    );
}

#[test]
fn test_missing_directory_is_rejected() {
    let tree = tree("basic");
    let missing = tree.path("no-such-dir");
    let message = call_error(
        &tree.runtime(),
        "Glob",
        Some(&missing),
        json!({ "pattern": "*", "path": missing }),
    );
    assert_eq!(message, format!("Error: Directory not found: {missing}"));
}

#[test]
fn test_missing_pattern_reports_source_message() {
    let tree = tree("basic");
    let message = call_error(
        &tree.runtime(),
        "Glob",
        Some(&tree.root_str()),
        json!({ "path": tree.root_str() }),
    );
    assert_eq!(
        message,
        "The 'pattern' parameter is required for the Glob tool."
    );
}

#[test]
fn test_bare_star_pattern_adds_soft_warning_but_executes() {
    let tree = tree("basic");
    let response = glob(&tree, "*", Some(&tree.root_str()));
    assert!(!response.is_error);
    assert!(
        response
            .text
            .starts_with("Note: Bare `*` matches files at any depth"),
        "{}",
        response.text
    );
    let recursive = glob(&tree, "**/*", Some(&tree.root_str()));
    assert!(
        recursive
            .text
            .starts_with("Note: `**/*` recursively expands the entire subtree"),
        "{}",
        recursive.text
    );
}

#[test]
fn test_result_limit_stops_collection_and_reports_it() {
    let tree = tree("basic");
    let bulk = tree.root.join("bulk");
    std::fs::create_dir_all(&bulk).expect("创建目录");
    for index in 0..1_005 {
        std::fs::write(bulk.join(format!("f{index:04}.txt")), "x\n").expect("写入");
    }
    let response = glob(&tree, "*.txt", Some(&bulk.to_string_lossy()));
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(field(&response, "count"), Some(json!(1_001)));
    assert_eq!(field(&response, "early_stopped"), Some(json!(true)));
    assert_eq!(field(&response, "truncated"), Some(json!(true)));
    assert!(
        response
            .text
            .contains("[Output truncated: 1001 files total (collection stopped at the result limit), showing first 1000]"),
        "{}",
        response.text
    );
    assert!(
        response.text.lines().count() >= 1_000,
        "内联应保留前 1000 条"
    );
}

#[test]
fn test_byte_limit_persists_full_output_and_inlines_head() {
    let tree = tree("basic");
    let bulk = tree.root.join("wide");
    std::fs::create_dir_all(&bulk).expect("创建目录");
    // 300 个长名文件：总量 ~3 万字节 > 20000 字节上限。
    for index in 0..300 {
        let name = format!("{index:03}_{}", "n".repeat(90));
        std::fs::write(bulk.join(format!("{name}.txt")), "x\n").expect("写入");
    }
    let response = glob(&tree, "*.txt", Some(&bulk.to_string_lossy()));
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(field(&response, "truncated"), Some(json!(true)));
    assert!(
        response.text.contains("exceeds 20000 byte limit"),
        "{}",
        &response.text[response.text.len().saturating_sub(200)..]
    );
    let persisted = field_str(&response, "persisted_path");
    assert!(
        persisted.contains(".local-mcp/artifacts/local-tool-output-"),
        "落盘路径: {persisted}"
    );
    let persisted_file = std::path::Path::new(&persisted);
    assert!(persisted_file.exists(), "落盘文件必须存在: {persisted}");
    let full = std::fs::read_to_string(persisted_file).expect("读取落盘文件");
    assert_eq!(full.lines().count(), 300, "落盘内容是完整结果");
}

#[test]
fn test_structured_fields_cover_glob_surface() {
    let tree = tree("basic");
    let response = glob(&tree, "**/*.rs", Some(&tree.root_str()));
    for key in [
        "tool",
        "ok",
        "truncated",
        "pattern",
        "search_root",
        "count",
        "early_stopped",
    ] {
        assert!(
            response.structured.get(key).is_some(),
            "结构化字段缺失 {key}: {}",
            response.structured
        );
    }
    assert_eq!(field_str(&response, "search_root"), tree.root_str());
}

#[test]
fn test_file_as_search_root_behaves_like_walkdir() {
    let tree = tree("basic");
    let file = tree.path("notes.txt");
    // 源实现用 walkdir 遍历「文件根」：只产出该条目本身，rel = ""（`*` 可命中空串）。
    let response = glob(&tree, "*", Some(&file));
    assert!(!response.is_error, "{}", response.text);
    assert!(
        response.text.ends_with(&file),
        "应返回该文件自身（`*` 软告警前缀除外）: {}",
        response.text
    );
    assert!(!response.text.contains("notes.txt/"), "不得多出结尾分隔符");

    // 具体文件名 pattern 与空串不匹配 → No files found.（与源实现一致）
    let response = glob(&tree, "notes.txt", Some(&file));
    assert_eq!(response.text, "No files found.");
}
