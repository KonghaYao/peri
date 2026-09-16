//! 截断与落盘：`truncate_bytes`、产物目录、提示文案与端到端回读（A-007「落盘」证据面）。

#[path = "fs_support/mod.rs"]
mod support;

use local_mcp_server::output::{truncate_bytes, Persister, DEFAULT_ARTIFACT_DIR};
use serde_json::json;

use support::{call, path_args, tree};

#[test]
fn test_truncate_bytes_never_splits_utf8() {
    assert_eq!(truncate_bytes("hello", 10), "hello");
    assert_eq!(truncate_bytes("hello", 5), "hello");
    assert_eq!(truncate_bytes("hello", 4), "hell");
    assert_eq!(truncate_bytes("日", 1), "");
    assert_eq!(truncate_bytes("日", 2), "");
    assert_eq!(truncate_bytes("日本", 3), "日");
    assert_eq!(truncate_bytes("aé", 2), "a");
    assert_eq!(truncate_bytes("", 0), "");
}

#[test]
fn test_persister_writes_into_private_artifact_directory_and_reports_host_readable_hint() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let outcome = Persister::default().persist(runtime.root(), "full payload\nsecond line\n");
    assert!(outcome.hint.starts_with("\n\n[Full output saved to "));
    assert!(outcome
        .hint
        .ends_with("use Read tool to view complete content]"));
    let path = outcome.path.expect("必须返回落盘路径");
    assert!(
        path.contains(&format!("/{DEFAULT_ARTIFACT_DIR}/local-tool-output-")),
        "{path}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("落盘文件可读"),
        "full payload\nsecond line\n"
    );
}

#[test]
fn test_persisted_output_can_be_read_back_through_the_read_tool() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    let payload = "line one\nline two\nline three\n";
    let outcome = Persister::default().persist(runtime.root(), payload);
    let path = outcome.path.expect("落盘路径");

    let response = call(&runtime, "Read", Some(&path), path_args(&path));
    assert!(
        !response.is_error,
        "落盘文件必须能被 Read 读回: {}",
        response.text
    );
    assert!(response.text.contains("line three"), "{}", response.text);
}

#[test]
fn test_artifact_directory_is_created_lazily_and_stays_inside_root() {
    let tree = tree("basic");
    let runtime = tree.runtime();
    assert!(!tree.root.join(DEFAULT_ARTIFACT_DIR).exists());
    let outcome = Persister::default().persist(runtime.root(), "x\n");
    assert!(tree.root.join(DEFAULT_ARTIFACT_DIR).is_dir());
    let path = outcome.path.expect("落盘路径");
    assert!(
        path.starts_with(&tree.root_str()),
        "落盘必须留在授权根内: {path}"
    );
}

#[test]
fn test_glob_truncation_persists_original_host_readable_path_via_broker() {
    // 端到端：落盘提示给出的路径必须是宿主可读的**根内**路径（见 fs_executor.rs 的同类验证）。
    let tree = tree("basic");
    let runtime = tree.runtime();
    let bulk = tree.root.join("wide");
    std::fs::create_dir_all(&bulk).expect("创建目录");
    for index in 0..300 {
        let name = format!("{index:03}_{}", "n".repeat(90));
        std::fs::write(bulk.join(format!("{name}.txt")), "x\n").expect("写入");
    }
    let response = call(
        &runtime,
        "Glob",
        Some(&bulk.to_string_lossy()),
        json!({ "pattern": "*.txt", "path": bulk.to_string_lossy() }),
    );
    assert!(!response.is_error, "{}", response.text);
    let persisted = response
        .structured
        .get("persisted_path")
        .and_then(|value| value.as_str())
        .expect("落盘路径");
    assert!(std::path::Path::new(persisted).exists(), "{persisted}");
    assert!(
        response.text.contains(persisted),
        "提示文本必须与结构化路径一致"
    );
}
