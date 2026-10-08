use super::{format_output, tests::Wire, MAX_BYTES};
use crate::mcp::client::McpClientPool;
use peri_acp_types::workspace_output::StoreOutputRequest;
use rmcp::{
    model::{CustomRequest, CustomResult},
    service::{RequestContext, RoleServer},
    ServerHandler,
};

const URI: &str =
    "peri-output://11111111-1111-4111-8111-111111111111/22222222-2222-4222-8222-222222222222";

struct LongPathStore {
    path: String,
}

impl ServerHandler for LongPathStore {
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, rmcp::ErrorData> {
        let input: StoreOutputRequest = serde_json::from_value(request.params.unwrap()).unwrap();
        Ok(CustomResult::new(serde_json::json!({
            "uri": URI, "path": self.path, "byte_length": input.content.len()
        })))
    }
}

#[tokio::test]
async fn oversized_and_escaped_receipt_paths_preserve_exact_uri_within_total_budget() {
    for path in [
        "/".to_string() + &"p".repeat(MAX_BYTES),
        "\"\n雪".repeat(MAX_BYTES),
    ] {
        let wire = Wire::connect(LongPathStore { path: path.clone() }).await;
        let pool = McpClientPool::new_empty();
        wire.install(&pool, "workspace", true);
        for error in [false, true] {
            let output = format_output(Some(&pool), None, "雪".repeat(MAX_BYTES), error).await;
            assert!(output.len() <= MAX_BYTES, "{}", output.len());
            assert!(output.contains(URI));
            assert!(output.contains("Full output saved"));
            assert!(output.contains("Path omitted"));
            assert!(!output.contains("path="));
            assert!(!output.contains("Read(file_path="));
        }
        pool.clients.write().clear();
        wire.close().await;
    }
}

#[tokio::test]
async fn ordinary_receipt_path_is_preserved_without_truncating_the_address() {
    let path = "/remote/完整输出.txt";
    let wire = Wire::connect(LongPathStore { path: path.into() }).await;
    let pool = McpClientPool::new_empty();
    wire.install(&pool, "workspace", true);
    let output = format_output(Some(&pool), None, "x".repeat(MAX_BYTES + 1), false).await;
    assert!(output.len() <= MAX_BYTES);
    assert!(output.contains(&format!("path={}", serde_json::json!(path))));
    assert!(output.contains(URI));
    pool.clients.write().clear();
    wire.close().await;
}
