use super::*;
use std::fs;

/// [候选] 拼错文件名（maiin.rs）走编辑距离回退命中 main.rs。
#[test]
fn test_typo_candidate_via_edit_distance() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::write(base.join("main.rs"), "").unwrap();
    fs::write(base.join("lib.rs"), "").unwrap();
    let hint = with_path_hint(
        "File not found.",
        base.to_str().unwrap(),
        &base.join("maiin.rs"),
    );
    assert!(hint.contains("Did you mean one of these paths"), "{hint}");
    assert!(hint.contains("main.rs"), "{hint}");
}

/// [候选] 相对路径按 cwd 解析（src/lb.rs → src/lib.rs）。
#[test]
fn test_relative_path_resolves_against_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(base.join("src").join("lib.rs"), "").unwrap();
    let hint = with_path_hint(
        "File not found.",
        base.to_str().unwrap(),
        Path::new("src/lb.rs"),
    );
    assert!(hint.contains("lib.rs"), "{hint}");
}

/// [兜底候选] 目标目录不存在时，回退扫描 cwd 一层子目录的条目。
#[test]
fn test_fallback_scans_cwd_subdirs() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::create_dir_all(base.join("module")).unwrap();
    fs::write(base.join("module").join("helper.rs"), "").unwrap();
    let hint = with_path_hint(
        "File not found.",
        base.to_str().unwrap(),
        &base.join("module").join("help.rs"),
    );
    // 目标 parent 不存在 → 主候选池为空 → 从 cwd 一层子目录收集到 helper.rs
    assert!(hint.contains("helper.rs"), "{hint}");
}

/// [保守] 无合格候选时不产生建议，原文本不变。
#[test]
fn test_no_candidates_keeps_base_text() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let hint = with_path_hint(
        "File not found.",
        base.to_str().unwrap(),
        &base.join("totally_different.xyz"),
    );
    assert_eq!(hint, "File not found.");
}

/// [性能] 200 项目录下候选生成 < 50ms。
#[test]
fn test_perf_under_50ms_in_large_dir() {
    use std::time::Instant;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    for i in 0..200 {
        fs::write(base.join(format!("file_{i:03}.rs")), "").unwrap();
    }
    let start = Instant::now();
    let hint = with_path_hint(
        "File not found.",
        base.to_str().unwrap(),
        &base.join("fle_100.rs"),
    );
    let elapsed = start.elapsed();
    assert!(hint.contains("file_100.rs"), "{hint}");
    assert!(elapsed.as_millis() < 50, "应该 < 50ms，实际: {elapsed:?}");
}

/// [工具集成] Read 的文件不存在错误附带候选；Edit 的文本片段失败不附带
/// （失败对象是文本而非路径，路径候选会让模型困惑）。
#[tokio::test]
async fn test_read_gets_path_hint_and_edit_text_failure_does_not() {
    use crate::filesystem::{EditFileTool, ReadFileTool};
    use peri_agent::tools::{BaseTool, ToolContext};
    use peri_mcp_common::failure::ToolFailure;

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::write(base.join("present.txt"), "actual text\n").unwrap();
    let cwd = base.to_str().unwrap();

    let read = ReadFileTool::new(cwd);
    let error = read
        .invoke(
            serde_json::json!({"file_path": "presentt.txt"}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    let recovery = &error.downcast_ref::<ToolFailure>().unwrap().recovery;
    assert!(
        recovery.contains("Did you mean one of these paths"),
        "{recovery}"
    );
    assert!(recovery.contains("present.txt"), "{recovery}");

    let edit = EditFileTool::new(cwd);
    // 文件存在、old_string 缺失：不得给路径候选
    let error = edit
        .invoke(
            serde_json::json!({"file_path": "present.txt", "old_string": "absent text", "new_string": "replacement"}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    let recovery = &error.downcast_ref::<ToolFailure>().unwrap().recovery;
    assert!(!recovery.contains("Did you mean"), "{recovery}");

    // 文件本身缺失：应给路径候选
    let error = edit
        .invoke(
            serde_json::json!({"file_path": "presentt.txt", "old_string": "actual text", "new_string": "replacement"}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    let recovery = &error.downcast_ref::<ToolFailure>().unwrap().recovery;
    assert!(
        recovery.contains("Did you mean one of these paths"),
        "{recovery}"
    );
    assert!(recovery.contains("present.txt"), "{recovery}");
    assert_eq!(
        fs::read_to_string(base.join("present.txt")).unwrap(),
        "actual text\n"
    );
}
