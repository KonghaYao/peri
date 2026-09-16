//! `Bash` 输出语义：合并、两层限额、UTF-8 边界与落盘（FC-BASH-02）。
//!
//! 事实源：`terminal.rs::{merge_output, truncate_output}`、
//! `async_tasks/shell.rs::{truncate_bytes, persist_truncated_output}`。

use std::sync::Arc;

use local_mcp_server::tasks::log::{DirOutputPersist, OutputPersist};
use local_mcp_server::tools::bash::{
    exceeds_limits, host_projection_note, merge_output, persist_partial_output, truncate_bytes,
    truncate_output, HOST_OUTPUT_CHAR_LIMIT, MAX_OUTPUT_CHARS, MAX_OUTPUT_LINES,
};

fn persist_in(dir: &tempfile::TempDir) -> Arc<dyn OutputPersist> {
    Arc::new(DirOutputPersist::new(dir.path()))
}

#[test]
fn merge_output_matches_source_format() {
    assert_eq!(merge_output("hello\n", "", Some(0)), "hello\n");
    assert_eq!(merge_output("", "boom", Some(0)), "[stderr]\nboom");
    assert_eq!(
        merge_output("out", "err", Some(2)),
        "out\n[stderr]\nerr\n[Exit code: 2]"
    );
    // 空输出占位（源两种形态）。
    assert_eq!(
        merge_output("", "", Some(0)),
        "[Command completed with exit code 0]"
    );
    assert_eq!(merge_output("", "", None), "[no output captured yet]");
    // 非零退出码即使无 stderr 也要标注。
    assert_eq!(merge_output("out", "", Some(3)), "out\n[Exit code: 3]");
}

#[test]
fn truncate_bytes_is_utf8_safe() {
    let text = "日本語テキスト";
    let truncated = truncate_bytes(text, 4);
    assert!(truncated.len() <= 4);
    assert!(text.starts_with(&truncated), "不得拆开多字节字符");
    assert_eq!(truncate_bytes(text, 1_000), text);
    // 边界正好落在字符内时回退到上一个边界。
    assert_eq!(truncate_bytes("é", 1).len(), 0);
    assert_eq!(truncate_bytes("é", 2), "é");
}

#[test]
fn small_output_is_untouched() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let persist = persist_in(&dir);
    let output = "line1\nline2\n";
    let shaped = truncate_output(output, persist.as_ref());
    assert_eq!(shaped.text, output);
    assert!(!shaped.truncated);
    assert_eq!(shaped.persisted_path, None);
    assert!(!exceeds_limits(output));
}

#[test]
fn line_limit_keeps_head_and_tail_and_persists_full_content() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let persist = persist_in(&dir);
    let lines: Vec<String> = (0..MAX_OUTPUT_LINES + 50)
        .map(|index| format!("line-{index}"))
        .collect();
    let output = lines.join("\n");
    assert!(exceeds_limits(&output));

    let shaped = truncate_output(&output, persist.as_ref());
    assert!(shaped.truncated);
    assert!(shaped.text.contains(
        "... [50 lines truncated, showing head 1000 and tail 1000 of 2050 total lines] ..."
    ));
    assert!(shaped.text.starts_with("line-0\n"), "头部保留");
    assert!(
        shaped.text.contains("\n\nline-1050\n"),
        "尾部从第 1050 行开始"
    );
    assert!(shaped.text.contains("line-2049\n"), "尾部保留到末行");
    assert!(
        shaped
            .text
            .trim_end()
            .ends_with("use Read tool to view complete content]"),
        "落盘提示在末尾"
    );

    let path = shaped.persisted_path.expect("应落盘");
    let persisted = std::fs::read_to_string(&path).expect("落盘文件");
    assert_eq!(persisted, output, "落盘必须是完整输出");
    assert!(path.contains("local-tool-output-"));
}

#[test]
fn byte_limit_truncates_and_persists() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let persist = persist_in(&dir);
    let output = "x".repeat(MAX_OUTPUT_CHARS + 100);
    assert!(exceeds_limits(&output));

    let shaped = truncate_output(&output, persist.as_ref());
    assert!(shaped.truncated);
    assert!(shaped.text.contains(&format!(
        "[Output truncated: exceeds {MAX_OUTPUT_CHARS} byte limit]"
    )));
    assert!(shaped.text.contains("[Full output saved to"));
    assert!(shaped.persisted_path.is_some());
}

#[test]
fn byte_limit_respects_utf8_boundaries() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let persist = persist_in(&dir);
    // 每个字符 3 字节，总字节数超过上限且边界落在字符中间。
    let output = "あ".repeat((MAX_OUTPUT_CHARS / 3) + 10);
    let shaped = truncate_output(&output, persist.as_ref());
    assert!(shaped.truncated);
    assert!(shaped.text.is_char_boundary(shaped.text.len()));
    let trimmed = shaped.text.split('\n').next().unwrap_or_default();
    assert!(trimmed.chars().all(|c| c == 'あ'), "不得出现替换字符");
}

#[test]
fn partial_output_hint_uses_source_wording() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let persist = persist_in(&dir);
    let outcome = persist_partial_output("partial text", persist.as_ref());
    assert!(
        outcome.hint.contains("[Partial output saved to "),
        "{}",
        outcome.hint
    );
    assert!(
        outcome
            .hint
            .contains("use Read tool to view captured output so far"),
        "{}",
        outcome.hint
    );
    let path = outcome.path.expect("应落盘");
    assert_eq!(std::fs::read_to_string(path).expect("内容"), "partial text");
}

#[test]
fn two_layer_limits_are_declared_not_conflated() {
    // 工具内部限额。
    assert_eq!(MAX_OUTPUT_CHARS, 65_000);
    assert_eq!(MAX_OUTPUT_LINES, 2_000);
    // 宿主投影（Peri Agent 层）事实，沙箱不执行、只登记。
    assert_eq!(HOST_OUTPUT_CHAR_LIMIT, 10_000);
    let note = host_projection_note();
    assert!(note.contains("10000"), "{note}");
    assert!(note.contains("宿主投影"), "{note}");
    assert!(note.contains("65000"), "{note}");
    assert_ne!(HOST_OUTPUT_CHAR_LIMIT, MAX_OUTPUT_CHARS);
}

#[test]
fn persistence_failure_degrades_without_losing_text() {
    // 落盘目标不可写（父路径是文件）→ 提示降级，但截断文本仍然返回。
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_as_dir = dir.path().join("not-a-dir");
    std::fs::write(&file_as_dir, "x").expect("占位文件");
    let persist = DirOutputPersist::new(file_as_dir.join("nested"));
    let output = "y".repeat(MAX_OUTPUT_CHARS + 10);
    let shaped = truncate_output(&output, &persist);
    assert!(shaped.truncated);
    assert!(shaped.text.contains("[Failed to save full output to "));
    assert_eq!(shaped.persisted_path, None);
}
