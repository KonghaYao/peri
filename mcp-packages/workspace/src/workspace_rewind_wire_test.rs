use std::time::Duration;

use rmcp::{
    model::{
        CallToolRequestParams, ClientRequest, CustomRequest, MetaObject, RequestMetaObject,
        ServerResult,
    },
    ServiceExt,
};

use crate::{WorkspaceMcpServer, TASK_OWNER_META_KEY};

fn owner_meta(owner: &str) -> serde_json::Value {
    serde_json::json!({(TASK_OWNER_META_KEY): owner})
}

fn call_meta(owner: &str, tool_call_id: &str) -> RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(TASK_OWNER_META_KEY.into(), owner.into());
    fields.insert(
        peri_mcp_common::invocation::INVOCATION_META_KEY.into(),
        serde_json::json!({"version": 2, "toolCallId": tool_call_id}),
    );
    RequestMetaObject(MetaObject(fields))
}

async fn write_file(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    file: &std::path::Path,
    content: &str,
    tool_call_id: &str,
) {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "file_path".into(),
        serde_json::Value::String(file.to_string_lossy().into_owned()),
    );
    arguments.insert("content".into(), serde_json::Value::String(content.into()));
    let mut request = CallToolRequestParams::new("Write").with_arguments(arguments);
    request.meta = Some(call_meta("session-a", tool_call_id));
    client.peer().call_tool_once(request).await.unwrap();
}

async fn rewind(
    client: &rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    changes: serde_json::Value,
    owner: Option<&str>,
) -> Result<ServerResult, rmcp::ServiceError> {
    let params = match owner {
        Some(owner) => {
            serde_json::json!({"changes": changes, "_meta": owner_meta(owner)})
        }
        None => serde_json::json!({"changes": changes}),
    };
    client
        .peer()
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            "workspace/rewindFiles",
            Some(params),
        )))
        .await
}

#[tokio::test]
async fn rewind_wire_restores_write_pre_image_and_keeps_generic_edit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("target.txt");
    tokio::fs::write(&file, "user original").await.unwrap();
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

    // Peri 宿主：Write 带 invocation + owner 元数据，Workspace 记录 pre-image。
    write_file(&client, &file, "agent output", "call-write").await;
    let changes = serde_json::json!([{
        "kind": "write",
        "path": "target.txt",
        "content": "agent output",
        "tool_call_id": "call-write",
    }]);
    let result = rewind(&client, changes.clone(), Some("session-a"))
        .await
        .unwrap();
    let ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    assert_eq!(result.0["ok"], true);
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "user original"
    );

    // 无 pre-image 的 Write 回退必须拒绝，不能删文件或回退 HEAD。
    tokio::fs::write(&file, "agent output").await.unwrap();
    assert!(
        rewind(&client, changes, None).await.is_err(),
        "generic Write rewind without a recorded pre-image must fail"
    );
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "agent output"
    );

    // Edit 的 old/new 自带 pre-image，通用 MCP 客户端仍可回退。
    tokio::fs::write(&file, "new").await.unwrap();
    let edit = serde_json::json!([{
        "kind": "edit",
        "path": "target.txt",
        "old_string": "old",
        "new_string": "new",
        "tool_call_id": "call-edit",
    }]);
    let result = rewind(&client, edit, None).await.unwrap();
    let ServerResult::CustomResult(result) = result else {
        panic!("custom result")
    };
    assert_eq!(result.0["ok"], true);
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "old");

    client
        .close_with_timeout(Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), server_task)
        .await
        .unwrap()
        .unwrap();
}
