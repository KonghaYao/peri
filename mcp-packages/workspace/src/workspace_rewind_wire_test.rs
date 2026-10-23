use std::time::Duration;

use rmcp::{
    model::{ClientRequest, CustomRequest, ServerResult},
    ServiceExt,
};

use crate::{WorkspaceMcpServer, TASK_OWNER_META_KEY};

#[tokio::test]
async fn rewind_wire_works_without_owner_and_with_owner() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("target.txt");
    tokio::fs::write(&file, "new").await.unwrap();
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false);
    let (client_io, server_io) = tokio::io::duplex(8192);
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
    let changes = serde_json::json!([{"kind":"write","path":"target.txt","content":"new"}]);
    // 通用 MCP 客户端：无任何 `peri/*` 元数据。
    let generic = CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({"changes": changes})),
    );
    let result = client
        .peer()
        .send_request(ClientRequest::CustomRequest(generic))
        .await
        .unwrap();
    let ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    assert_eq!(result.0["ok"], true);
    assert!(!file.exists());
    // Peri 宿主：owner 存在时按 owner 准入，计算仍在服务端。
    tokio::fs::write(&file, "new").await.unwrap();
    let meta = serde_json::json!({(TASK_OWNER_META_KEY): "session-a"});
    let owned = CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({"changes": changes, "_meta": meta})),
    );
    let result = client
        .peer()
        .send_request(ClientRequest::CustomRequest(owned))
        .await
        .unwrap();
    let ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    assert_eq!(result.0["ok"], true);
    assert!(!file.exists());
    client
        .close_with_timeout(Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), server_task)
        .await
        .unwrap()
        .unwrap();
}
