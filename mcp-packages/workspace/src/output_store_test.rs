use peri_acp_types::workspace_output::{StoreOutputRequest, StoredOutput, STORE_OUTPUT_METHOD};
use rmcp::{
    model::{
        CallToolRequestParams, ClientRequest, CustomRequest, ReadResourceRequestParams,
        ResourceContents, ServerResult,
    },
    ServiceExt,
};

use super::OutputStore;
use crate::WorkspaceMcpServer;

#[test]
fn output_references_are_instance_bound_and_failures_publish_nothing() {
    let store = OutputStore::new();
    let content = "完整输出\nsecond line";
    let result = store
        .store(StoreOutputRequest {
            content: content.into(),
        })
        .unwrap();
    let output: StoredOutput = serde_json::from_value(result.0).unwrap();
    assert_eq!(output.byte_length, content.len() as u64);
    assert_eq!(store.read(&output.uri).unwrap(), content);
    assert!(OutputStore::new().read(&output.uri).is_err());
    assert!(store.read(&format!("{}/../secret", output.uri)).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&output.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&store.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    std::fs::remove_dir_all(&store.directory).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, "blocked").unwrap();
    let mut failing = OutputStore::new();
    failing.directory = blocked;
    assert!(failing
        .store(StoreOutputRequest {
            content: content.into()
        })
        .is_err());
    assert!(failing.artifacts.lock().is_empty());
}

#[tokio::test]
async fn output_wire_stores_and_reads_in_the_tool_environment() {
    let tool_cwd = tempfile::tempdir().unwrap();
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = WorkspaceMcpServer::new(tool_cwd.path().to_string_lossy().into_owned(), None);
    let server_task = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap()
    });
    let client = ().serve(client_io).await.unwrap();
    let content = "远程完整输出\n".repeat(2100);
    let response = client
        .peer()
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            STORE_OUTPUT_METHOD,
            Some(serde_json::json!({ "content": content })),
        )))
        .await
        .unwrap();
    let ServerResult::CustomResult(result) = response else {
        panic!("expected stored output")
    };
    let output: StoredOutput = serde_json::from_value(result.0).unwrap();
    assert!(output.uri.starts_with("peri-output://"));
    assert_eq!(output.byte_length, content.len() as u64);
    let response = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(&output.uri))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents { text, .. } = &response.contents[0] else {
        panic!("expected text")
    };
    assert_eq!(text, &content);
    let page = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("Read").with_arguments(
                serde_json::json!({ "file_path": output.path, "offset": 2099, "limit": 2 })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(page.is_error, Some(true));
    assert!(serde_json::to_string(&page)
        .unwrap()
        .contains("远程完整输出"));
    assert_eq!(client.peer().list_all_tools().await.unwrap().len(), 7);
    assert!(client
        .peer()
        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
            STORE_OUTPUT_METHOD,
            Some(serde_json::json!({})),
        )))
        .await
        .is_err());
    client.cancel().await.unwrap();
    server_task.await.unwrap();
    assert_eq!(std::fs::read_to_string(&output.path).unwrap(), content);
    std::fs::remove_dir_all(std::path::Path::new(&output.path).parent().unwrap()).unwrap();
}
