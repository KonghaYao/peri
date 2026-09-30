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
