use super::*;
use crate::rewind_snapshot::{RewindSnapshots, Snapshot};

fn request(changes: serde_json::Value) -> CustomRequest {
    CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({"changes": changes})),
    )
}

fn seed(
    snapshots: &RewindSnapshots,
    owner: Option<&str>,
    tool_call_id: &str,
    path: std::path::PathBuf,
    pre: Option<&str>,
    post: &str,
) {
    snapshots.record(
        owner,
        tool_call_id,
        Snapshot {
            path,
            pre: pre.map(|text| text.as_bytes().to_vec()),
            post: post.as_bytes().to_vec(),
        },
    );
}

async fn canonical(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path).await.unwrap()
}

#[tokio::test]
async fn test_rewind_write_restores_untracked_pre_image() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("notes.txt");
    tokio::fs::write(&file, "user original").await.unwrap();
    // Tool Write overwrote the user's untracked file.
    tokio::fs::write(&file, "agent output").await.unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-1",
        canonical(&file).await,
        Some("user original"),
        "agent output",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "notes.txt",
        "content": "agent output",
        "tool_call_id": "call-1",
    }]);
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "user original",
        "rewind must restore the pre-write content, not delete the file"
    );
}

#[tokio::test]
async fn test_rewind_write_restores_dirty_tracked_working_tree() {
    let remote = tempfile::tempdir().unwrap();
    let root = remote.path();
    tokio::fs::write(root.join("tracked.txt"), "committed base")
        .await
        .unwrap();
    for args in [vec!["init", "--quiet"], vec!["add", "tracked.txt"]] {
        let status = tokio::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .await
            .unwrap();
        assert!(status.success());
    }
    let status = tokio::process::Command::new("git")
        .args([
            "-c",
            "user.name=Peri Test",
            "-c",
            "user.email=peri@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ])
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .await
        .unwrap();
    assert!(status.success());
    // User had uncommitted edits before the tool overwrote the file.
    tokio::fs::write(root.join("tracked.txt"), "user draft")
        .await
        .unwrap();
    tokio::fs::write(root.join("tracked.txt"), "agent output")
        .await
        .unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-2",
        canonical(&root.join("tracked.txt")).await,
        Some("user draft"),
        "agent output",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "tracked.txt",
        "content": "agent output",
        "tool_call_id": "call-2",
    }]);
    rewind_files(root.to_str().unwrap(), &snapshots, None, request(changes))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("tracked.txt"))
            .await
            .unwrap(),
        "user draft",
        "rewind must restore the pre-write working tree, not HEAD"
    );
}

#[tokio::test]
async fn test_rewind_write_created_file_is_removed() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("created.txt");
    tokio::fs::write(&file, "agent output").await.unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-3",
        canonical(&file).await,
        None,
        "agent output",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "created.txt",
        "content": "agent output",
        "tool_call_id": "call-3",
    }]);
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap();
    assert!(!file.exists(), "a file created by Write is removed again");
}

#[tokio::test]
async fn test_rewind_write_append_restores_pre_append_content() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("log.txt");
    tokio::fs::write(&file, "user original").await.unwrap();
    tokio::fs::write(&file, "user originalappended")
        .await
        .unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-4",
        canonical(&file).await,
        Some("user original"),
        "user originalappended",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "log.txt",
        "content": "appended",
        "tool_call_id": "call-4",
    }]);
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "user original"
    );
}

#[tokio::test]
async fn test_rewind_write_uses_workspace_root_and_checks_external_change() {
    let remote = tempfile::tempdir().unwrap();
    let host = tempfile::tempdir().unwrap();
    tokio::fs::write(remote.path().join("same.txt"), "written remotely")
        .await
        .unwrap();
    tokio::fs::write(host.path().join("same.txt"), "host copy")
        .await
        .unwrap();
    let snapshots = RewindSnapshots::default();
    let path = canonical(&remote.path().join("same.txt")).await;
    seed(
        &snapshots,
        None,
        "call-5",
        path,
        Some("user copy"),
        "written remotely",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "same.txt",
        "content": "written remotely",
        "tool_call_id": "call-5",
    }]);
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(remote.path().join("same.txt"))
            .await
            .unwrap(),
        "user copy"
    );
    assert_eq!(
        tokio::fs::read_to_string(host.path().join("same.txt"))
            .await
            .unwrap(),
        "host copy"
    );
    tokio::fs::write(remote.path().join("same.txt"), "externally edited")
        .await
        .unwrap();
    let error = rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("changed externally"));
    assert_eq!(
        tokio::fs::read_to_string(remote.path().join("same.txt"))
            .await
            .unwrap(),
        "externally edited"
    );
}

#[tokio::test]
async fn test_rewind_batch_preflight_failure_keeps_prior_file_and_rejects_escape() {
    let remote = tempfile::tempdir().unwrap();
    tokio::fs::write(remote.path().join("a.txt"), "new-a")
        .await
        .unwrap();
    tokio::fs::write(remote.path().join("b.txt"), "unexpected-b")
        .await
        .unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-a",
        canonical(&remote.path().join("a.txt")).await,
        Some("old-a"),
        "new-a",
    );
    seed(
        &snapshots,
        None,
        "call-b",
        canonical(&remote.path().join("b.txt")).await,
        Some("old-b"),
        "expected-b",
    );
    let changes = serde_json::json!([
        {"kind":"write","path":"b.txt","content":"expected-b","tool_call_id":"call-b"},
        {"kind":"write","path":"a.txt","content":"new-a","tool_call_id":"call-a"}
    ]);
    assert!(rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes)
    )
    .await
    .is_err());
    assert_eq!(
        tokio::fs::read_to_string(remote.path().join("a.txt"))
            .await
            .unwrap(),
        "new-a"
    );
    let escape = serde_json::json!([{"kind":"edit","path":"../elsewhere","old_string":"a","new_string":"b","tool_call_id":"call-x"}]);
    assert!(rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(escape)
    )
    .await
    .is_err());
}

#[tokio::test]
async fn test_rewind_edit_uses_snapshot_for_replace_all() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("c.txt");
    tokio::fs::write(&file, "x x x").await.unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-6",
        canonical(&file).await,
        Some("a a a"),
        "x x x",
    );
    let changes = serde_json::json!([{
        "kind": "edit",
        "path": "c.txt",
        "old_string": "a",
        "new_string": "x",
        "tool_call_id": "call-6",
    }]);
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "a a a");
}

#[tokio::test]
async fn test_rewind_edit_without_snapshot_reverses_multibyte_content() {
    let remote = tempfile::tempdir().unwrap();
    tokio::fs::write(remote.path().join("c.txt"), "前🚀新后")
        .await
        .unwrap();
    let snapshots = RewindSnapshots::default();
    let changes = serde_json::json!([{
        "kind": "edit",
        "path": "c.txt",
        "old_string": "旧",
        "new_string": "新",
        "tool_call_id": "call-legacy",
    }]);
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(remote.path().join("c.txt"))
            .await
            .unwrap(),
        "前🚀旧后"
    );
}

#[tokio::test]
async fn test_rewind_write_without_pre_image_refuses_without_mutation() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("legacy.txt");
    tokio::fs::write(&file, "current").await.unwrap();
    let snapshots = RewindSnapshots::default();
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "legacy.txt",
        "content": "current",
        "tool_call_id": "call-unknown",
    }]);
    let error = rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("pre-image unavailable"));
    assert_eq!(tokio::fs::read_to_string(file).await.unwrap(), "current");
}

#[tokio::test]
async fn test_rewind_write_without_call_identity_refuses() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("legacy.txt");
    tokio::fs::write(&file, "current").await.unwrap();
    let snapshots = RewindSnapshots::default();
    let changes = serde_json::json!([{"kind":"write","path":"legacy.txt","content":"current"}]);
    let error = rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("lacks call identity"));
    assert_eq!(tokio::fs::read_to_string(file).await.unwrap(), "current");
}

#[tokio::test]
async fn test_rewind_write_payload_mismatch_refuses() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("mismatch.txt");
    tokio::fs::write(&file, "current").await.unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        None,
        "call-7",
        canonical(&file).await,
        Some("old"),
        "different output",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "mismatch.txt",
        "content": "current",
        "tool_call_id": "call-7",
    }]);
    let error = rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        None,
        request(changes),
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match recorded pre-image"));
    assert_eq!(tokio::fs::read_to_string(file).await.unwrap(), "current");
}

#[tokio::test]
async fn test_rewind_snapshot_is_session_scoped() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("scoped.txt");
    tokio::fs::write(&file, "agent output").await.unwrap();
    let snapshots = RewindSnapshots::default();
    seed(
        &snapshots,
        Some("session-a"),
        "call-8",
        canonical(&file).await,
        Some("user original"),
        "agent output",
    );
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "scoped.txt",
        "content": "agent output",
        "tool_call_id": "call-8",
    }]);
    let error = rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        Some("session-b"),
        request(changes.clone()),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("pre-image unavailable"));
    rewind_files(
        remote.path().to_str().unwrap(),
        &snapshots,
        Some("session-a"),
        request(changes),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(file).await.unwrap(),
        "user original"
    );
}
