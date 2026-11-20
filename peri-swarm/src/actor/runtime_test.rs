//! Remote/local dispatch for a child's session store (S6b).
//!
//! The remote branch needs a live Loom (verified in S6b2); these tests only pin
//! the refusal to degrade: a half-configured remote identity must not fall back
//! to the local SQLite file.

use std::path::PathBuf;

use uuid::Uuid;

use super::{open_session_resources, ChildConfig, ChildError, SessionStoreDeployment};

fn config(machine_id: Option<Uuid>, workspace_root: Option<PathBuf>) -> ChildConfig {
    ChildConfig {
        cwd: PathBuf::from("/tmp/peri-swarm-runtime-test"),
        session_store: SessionStoreDeployment::default_local(),
        machine_id,
        workspace_root,
        settings_json: None,
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        root_session_id: "0192f0c4-0000-7000-8000-000000000002".to_owned(),
    }
}

async fn expect_partial_identity_error(machine_id: Option<Uuid>, workspace_root: Option<PathBuf>) {
    let error = open_session_resources(&config(machine_id, workspace_root))
        .await
        .expect_err("a partial remote identity must not open any store");
    match error {
        ChildError::Local(message) => {
            assert!(message.contains("machine_id"), "{message}");
            assert!(message.contains("workspace_root"), "{message}");
            assert!(message.contains("refusing"), "{message}");
        }
        other => panic!("expected a local configuration error, got {other:?}"),
    }
}

#[tokio::test]
async fn machine_id_without_workspace_root_is_an_explicit_error() {
    expect_partial_identity_error(Some(Uuid::now_v7()), None).await;
}

#[tokio::test]
async fn workspace_root_without_machine_id_is_an_explicit_error() {
    expect_partial_identity_error(None, Some(PathBuf::from("/tmp/root"))).await;
}

// ── S8a2: `session/new.mcpServers` wire shape ───────────────────────────────

use serde_json::json;

use crate::domain::{McpHeader, McpToolKind, McpToolSpec};

use super::{session_new_params, META_ALLOWED_TOOLS, META_EXTRA, META_REQUIRE_APPROVAL};

fn declared_tools() -> Vec<McpToolSpec> {
    let mut extra = serde_json::Map::new();
    extra.insert("defer_loading".to_owned(), json!(true));
    vec![
        McpToolSpec {
            kind: McpToolKind::Mcp,
            server_label: "deepwiki".to_owned(),
            server_url: "https://mcp.example.test/mcp".to_owned(),
            headers: vec![
                McpHeader {
                    name: "Authorization".to_owned(),
                    value: "Bearer caller".to_owned(),
                },
                McpHeader {
                    name: "X-Tenant".to_owned(),
                    value: "acme".to_owned(),
                },
            ],
            require_approval: Some(json!({"never": {"tool_names": ["read_wiki"]}})),
            allowed_tools: Some(json!(["read_wiki", "search"])),
            extra,
        },
        McpToolSpec::new("plain", "http://127.0.0.1:1/mcp"),
    ]
}

#[test]
fn declared_tools_become_http_servers_with_every_openai_field_in_meta() {
    let params = session_new_params(
        "/tmp/ws",
        json!({"peri.rootSessionId": "0192f0c4-0000-7000-8000-000000000002"}),
        &declared_tools(),
    );

    assert_eq!(
        params["mcpServers"],
        json!([
            {
                "type": "http",
                "name": "deepwiki",
                "url": "https://mcp.example.test/mcp",
                "headers": [
                    {"name": "Authorization", "value": "Bearer caller"},
                    {"name": "X-Tenant", "value": "acme"},
                ],
                "_meta": {
                    META_REQUIRE_APPROVAL: {"never": {"tool_names": ["read_wiki"]}},
                    META_ALLOWED_TOOLS: ["read_wiki", "search"],
                    META_EXTRA: {"defer_loading": true},
                },
            },
            {
                "type": "http",
                "name": "plain",
                "url": "http://127.0.0.1:1/mcp",
                "headers": [],
            },
        ])
    );
    assert_eq!(
        params["_meta"],
        json!({"peri.rootSessionId": "0192f0c4-0000-7000-8000-000000000002"}),
        "the session meta block is unchanged by the injection"
    );
    assert_eq!(params["cwd"], json!("/tmp/ws"));
}

#[test]
fn no_declaration_keeps_session_new_without_the_mcp_servers_key() {
    let params = session_new_params(
        "/tmp/ws",
        json!({"peri.rootSessionId": "0192f0c4-0000-7000-8000-000000000002"}),
        &[],
    );
    assert_eq!(
        params,
        json!({
            "cwd": "/tmp/ws",
            "_meta": {"peri.rootSessionId": "0192f0c4-0000-7000-8000-000000000002"},
        }),
        "no tools must keep the S4 request byte-identical"
    );
}
