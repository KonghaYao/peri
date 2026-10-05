use super::*;

fn request(changes: serde_json::Value) -> CustomRequest {
    CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({"changes": changes})),
    )
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
    let changes =
        serde_json::json!([{"kind":"write","path":"same.txt","content":"written remotely"}]);
    rewind_files(remote.path().to_str().unwrap(), request(changes.clone()))
        .await
        .unwrap();
    assert!(!remote.path().join("same.txt").exists());
    assert_eq!(
        tokio::fs::read_to_string(host.path().join("same.txt"))
            .await
            .unwrap(),
        "host copy"
    );
    tokio::fs::write(remote.path().join("same.txt"), "externally edited")
        .await
        .unwrap();
    let error = rewind_files(remote.path().to_str().unwrap(), request(changes))
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
    let changes = serde_json::json!([
        {"kind":"write","path":"b.txt","content":"expected-b"},
        {"kind":"write","path":"a.txt","content":"new-a"}
    ]);
    assert!(
        rewind_files(remote.path().to_str().unwrap(), request(changes))
            .await
            .is_err()
    );
    assert_eq!(
        tokio::fs::read_to_string(remote.path().join("a.txt"))
            .await
            .unwrap(),
        "new-a"
    );
    let escape = serde_json::json!([{"kind":"edit","path":"../elsewhere","old_string":"a","new_string":"b"}]);
    assert!(
        rewind_files(remote.path().to_str().unwrap(), request(escape))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn test_rewind_edit_reverses_multibyte_content() {
    let remote = tempfile::tempdir().unwrap();
    tokio::fs::write(remote.path().join("c.txt"), "前🚀新后")
        .await
        .unwrap();
    let changes =
        serde_json::json!([{"kind":"edit","path":"c.txt","old_string":"旧","new_string":"新"}]);
    rewind_files(remote.path().to_str().unwrap(), request(changes))
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
async fn test_rewind_write_missing_history_content_refuses_without_mutation() {
    let remote = tempfile::tempdir().unwrap();
    let file = remote.path().join("legacy.txt");
    tokio::fs::write(&file, "current").await.unwrap();
    let changes = serde_json::json!([{"kind":"write","path":"legacy.txt","content":null}]);
    let error = rewind_files(remote.path().to_str().unwrap(), request(changes))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("lacks content"));
    assert_eq!(tokio::fs::read_to_string(file).await.unwrap(), "current");
}

#[tokio::test]
async fn test_rewind_tracked_write_restores_workspace_head() {
    let remote = tempfile::tempdir().unwrap();
    let root = remote.path();
    tokio::fs::create_dir(root.join("nested")).await.unwrap();
    for args in [vec!["init", "--quiet"], vec!["add", "nested/tracked.txt"]] {
        if args[0] == "add" {
            tokio::fs::write(root.join("nested/tracked.txt"), "original")
                .await
                .unwrap();
        }
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
    tokio::fs::write(root.join("nested/tracked.txt"), "replacement")
        .await
        .unwrap();
    let changes =
        serde_json::json!([{"kind":"write","path":"nested/tracked.txt","content":"replacement"}]);
    rewind_files(root.to_str().unwrap(), request(changes))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("nested/tracked.txt"))
            .await
            .unwrap(),
        "original"
    );
}
