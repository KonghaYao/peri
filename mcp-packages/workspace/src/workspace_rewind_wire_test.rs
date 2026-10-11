use std::time::Duration;

use rmcp::{
    model::{ClientRequest, CustomRequest, ServerResult},
    ServiceExt,
};

use crate::{TaskScopeAuthority, WorkspaceMcpServer, TASK_SCOPE_META_KEY};

#[tokio::test]
async fn rewind_wire_requires_session_scope_without_execution_fencing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("target.txt");
    tokio::fs::write(&file, "new").await.unwrap();
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy(), false)
        .with_task_scope_authority(TaskScopeAuthority::trusted_connection());
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
    let no_scope = CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({"changes": changes})),
    );
    assert!(client
        .peer()
        .send_request(ClientRequest::CustomRequest(no_scope))
        .await
        .is_err());
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new");
    let authority = TaskScopeAuthority::trusted_connection();
    let token = authority.issue("session-a");
    let meta = serde_json::json!({(TASK_SCOPE_META_KEY): token});
    let request = CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({"changes": changes,"_meta": meta})),
    );
    let result = client
        .peer()
        .send_request(ClientRequest::CustomRequest(request))
        .await
        .unwrap();
    let ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    assert_eq!(result.0["ok"], true);
    assert!(!file.exists());
    tokio::fs::write(&file, "new").await.unwrap();
    let second_token = authority.issue("session-a");
    let second_meta = serde_json::json!({(TASK_SCOPE_META_KEY): second_token});
    let second = CustomRequest::new(
        "workspace/rewindFiles",
        Some(serde_json::json!({
            "changes": [{"kind":"write","path":"target.txt","content":"new"}], "_meta": second_meta
        })),
    );
    client
        .peer()
        .send_request(ClientRequest::CustomRequest(second))
        .await
        .unwrap();
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
