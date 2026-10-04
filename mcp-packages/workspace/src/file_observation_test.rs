use std::time::Duration;

use rmcp::{
    model::{ClientRequest, CustomRequest, ServerResult},
    ServiceExt,
};

use crate::WorkspaceMcpServer;

#[tokio::test]
async fn read_text_wire_is_live_complete_and_cwd_bound() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("target.txt");
    std::fs::write(&file, "旧内容\n").unwrap();
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = WorkspaceMcpServer::new(dir.path().to_string_lossy().into_owned(), None);
    let server_task = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap()
    });
    let mut client = tokio::time::timeout(Duration::from_secs(2), ().serve(client_io))
        .await
        .unwrap()
        .unwrap();
    for (path, expected) in [
        ("target.txt".to_string(), "旧内容\n".to_string()),
        (file.to_string_lossy().into_owned(), "新内容".repeat(20000)),
    ] {
        std::fs::write(&file, &expected).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client
                .peer()
                .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                    "workspace/readText",
                    Some(serde_json::json!({"path": path})),
                ))),
        )
        .await
        .unwrap()
        .unwrap();
        let ServerResult::CustomResult(result) = result else {
            panic!("expected custom response")
        };
        assert_eq!(result.0["text"], expected);
    }
    client
        .close_with_timeout(Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), server_task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn invalid_missing_and_non_utf8_reads_do_not_expose_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("binary"), [0xff]).unwrap();
    for params in [
        serde_json::json!({"path": "missing"}),
        serde_json::json!({"path": "binary"}),
        serde_json::json!({"path": 5}),
        serde_json::json!({"path": ""}),
    ] {
        let error = super::read_text(
            dir.path().to_str().unwrap(),
            CustomRequest::new("workspace/readText", Some(params)),
        )
        .await
        .unwrap_err();
        assert!(!error.message.contains(dir.path().to_str().unwrap()));
    }
}

#[tokio::test]
async fn mention_reads_workspace_range_and_limits_directory_entries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let result = super::read_mention(
        dir.path().to_str().unwrap(),
        CustomRequest::new(
            "workspace/readMention",
            Some(serde_json::json!({"path": "target.txt", "lineStart": 2, "lineEnd": 3})),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.0["content"], "two\nthree");
    assert_eq!(result.0["lineStart"], 2);
    assert_eq!(result.0["lineEnd"], 3);
    for index in 0..105 {
        std::fs::write(dir.path().join(format!("file{index:03}")), "content").unwrap();
    }
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let result = super::read_mention(
        dir.path().to_str().unwrap(),
        CustomRequest::new(
            "workspace/readMention",
            Some(serde_json::json!({"path": "."})),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.0["isDir"], true);
    assert_eq!(result.0["truncated"], true);
    assert_eq!(result.0["content"].as_str().unwrap().lines().count(), 100);
    assert!(result.0["content"].as_str().unwrap().contains("file099"));
    assert!(!result.0["content"].as_str().unwrap().contains("file100"));
}

#[tokio::test]
async fn mention_rejects_traversal_and_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "outside secret").unwrap();
    let outside_file = outside.path().join("secret");
    for path in ["../secret", outside_file.to_str().unwrap()] {
        let result = super::read_mention(
            root.path().to_str().unwrap(),
            CustomRequest::new(
                "workspace/readMention",
                Some(serde_json::json!({"path": path})),
            ),
        )
        .await;
        assert!(result.is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("link"))
            .unwrap();
        let result = super::read_mention(
            root.path().to_str().unwrap(),
            CustomRequest::new(
                "workspace/readMention",
                Some(serde_json::json!({"path": "link"})),
            ),
        )
        .await;
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn mention_caps_long_file_at_2000_lines() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("large.txt"), "line\n".repeat(2001)).unwrap();
    let result = super::read_mention(
        root.path().to_str().unwrap(),
        CustomRequest::new(
            "workspace/readMention",
            Some(serde_json::json!({"path": "large.txt"})),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.0["truncated"], true);
    assert!(result.0["content"]
        .as_str()
        .unwrap()
        .ends_with("... (truncated)"));
}

#[tokio::test]
async fn mention_wire_reads_the_bound_workspace_file() {
    let host = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(host.path().join("same.txt"), "host content").unwrap();
    std::fs::write(workspace.path().join("same.txt"), "workspace content").unwrap();
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = WorkspaceMcpServer::new(workspace.path().to_string_lossy().into_owned(), None);
    let server_task = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap()
    });
    let mut client = ().serve(client_io).await.unwrap();
    let response = client
        .peer()
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            "workspace/readMention",
            Some(serde_json::json!({"path": "same.txt"})),
        )))
        .await
        .unwrap();
    let ServerResult::CustomResult(response) = response else {
        panic!("expected workspace mention response")
    };
    assert_eq!(response.0["content"], "workspace content");
    client
        .close_with_timeout(Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), server_task)
        .await
        .unwrap()
        .unwrap();
}
