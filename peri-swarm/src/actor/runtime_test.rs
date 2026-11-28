//! Remote/local dispatch for a child's session store (S6b).
//!
//! The remote branch needs a live PostgreSQL server (verified in S6b2); these
//! tests only pin the refusal to degrade: a half-configured remote identity must
//! not fall back to the local SQLite file, and an injected deployment handle is
//! used as-is without opening anything.

use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::session_resources::SessionResources;
use peri_resources::Resources;
use uuid::Uuid;

use super::{resolve_session_resources, ChildConfig, ChildError, SessionStoreDeployment};

fn config(
    machine_id: Option<Uuid>,
    workspace_root: Option<PathBuf>,
    session_store: SessionStoreDeployment,
    session_resources: Option<Arc<dyn SessionResources>>,
) -> ChildConfig {
    ChildConfig {
        cwd: PathBuf::from("/tmp/peri-swarm-runtime-test"),
        session_store,
        machine_id,
        workspace_root,
        settings_json: None,
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        workspace: None,
        root_session_id: "0192f0c4-0000-7000-8000-000000000002".to_owned(),
        session_resources,
    }
}

fn blocking_local_store(directory: &std::path::Path) -> SessionStoreDeployment {
    // The parent is a regular file, so opening `<file>/…` fails on every
    // platform if the resolver ignored the injected handle.
    let blocker = directory.join("not-a-directory");
    std::fs::write(&blocker, b"blocker").expect("write the blocking file");
    SessionStoreDeployment::local_path(blocker.join("peri-swarm-injected.db"))
}

async fn shared_local_resources(path: PathBuf) -> Arc<dyn SessionResources> {
    Resources::open_deployment(&SessionStoreDeployment::local_path(path))
        .await
        .expect("open the shared local store")
        .into_parts()
        .0
}

async fn expect_partial_identity_error(
    machine_id: Option<Uuid>,
    workspace_root: Option<PathBuf>,
    session_resources: Option<Arc<dyn SessionResources>>,
) {
    let error = resolve_session_resources(&config(
        machine_id,
        workspace_root,
        SessionStoreDeployment::default_local(),
        session_resources,
    ))
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
    expect_partial_identity_error(Some(Uuid::now_v7()), None, None).await;
}

#[tokio::test]
async fn workspace_root_without_machine_id_is_an_explicit_error() {
    expect_partial_identity_error(None, Some(PathBuf::from("/tmp/root")), None).await;
}

#[tokio::test]
async fn a_half_configured_identity_still_fails_closed_with_an_injected_handle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let shared = shared_local_resources(tmp.path().join("shared.db")).await;
    expect_partial_identity_error(Some(Uuid::now_v7()), None, Some(shared.clone())).await;
    expect_partial_identity_error(None, Some(PathBuf::from("/tmp/root")), Some(shared)).await;
}

#[tokio::test]
async fn an_injected_handle_is_used_without_opening_a_store_and_has_no_close_right() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let shared = shared_local_resources(tmp.path().join("shared.db")).await;
    let resolved = resolve_session_resources(&config(
        None,
        None,
        blocking_local_store(tmp.path()),
        Some(Arc::clone(&shared)),
    ))
    .await
    .expect("the injected handle is used as-is");
    assert!(
        Arc::ptr_eq(&resolved.resources, &shared),
        "the child must receive the deployment's exact facade"
    );
    assert!(
        resolved.shutdown.is_none(),
        "a child never owns the shared store's close right"
    );
}

#[tokio::test]
async fn local_mode_without_injection_still_opens_one_store_per_child() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let first = resolve_session_resources(&config(
        None,
        None,
        SessionStoreDeployment::local_path(tmp.path().join("first.db")),
        None,
    ))
    .await
    .expect("open the first local store");
    let second = resolve_session_resources(&config(
        None,
        None,
        SessionStoreDeployment::local_path(tmp.path().join("second.db")),
        None,
    ))
    .await
    .expect("open the second local store");
    assert!(
        !Arc::ptr_eq(&first.resources, &second.resources),
        "without injection each child still opens its own store"
    );
    assert!(first.shutdown.is_some());
    assert!(second.shutdown.is_some());
}

// ── S8a2: `session/new.mcpServers` wire shape ───────────────────────────────

use serde_json::json;

use crate::domain::{McpHeader, McpToolKind, McpToolSpec};
use crate::sandbox::SandboxMcpBinding;

use super::{
    session_load_params, session_new_params, META_ALLOWED_TOOLS, META_EXTRA, META_REQUIRE_APPROVAL,
};

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
        None,
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
        None,
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

#[test]
fn workspace_binding_becomes_the_first_single_http_server() {
    let binding = SandboxMcpBinding {
        logical_env_id: "env_01".to_owned(),
        sandbox_id: "sbx_0123456789abcdef0123456789abcdef".to_owned(),
        workspace_path: std::path::PathBuf::from("/tmp/sandbox/workspace"),
        mcp_url: "http://127.0.0.1:8081/sandboxes/env_01/mcp".to_owned(),
        token: Some("outer-secret".to_owned()),
    };
    let params = session_new_params(
        "/tmp/ws",
        json!({"peri.rootSessionId": "0192f0c4-0000-7000-8000-000000000002"}),
        Some(&binding),
        &declared_tools(),
    );
    let servers = params["mcpServers"].as_array().expect("mcpServers");
    assert_eq!(servers.len(), 3);
    assert_eq!(
        servers[0],
        json!({
            "type": "http",
            "name": "workspace",
            "url": "http://127.0.0.1:8081/sandboxes/env_01/mcp",
            "headers": [{"name": "Authorization", "value": "Bearer outer-secret"}],
            "_meta": {"peri.swarmSandbox": true},
        })
    );
    assert_eq!(servers[1]["name"], json!("deepwiki"));
    assert_eq!(servers[2]["name"], json!("plain"));
}

#[test]
fn a_workspace_binding_without_outer_token_omits_authorization() {
    let binding = SandboxMcpBinding {
        logical_env_id: "env_02".to_owned(),
        sandbox_id: "sbx_0123456789abcdef0123456789abcdef".to_owned(),
        workspace_path: std::path::PathBuf::from("/tmp/sandbox/workspace"),
        mcp_url: "http://127.0.0.1:8081/sandboxes/env_02/mcp".to_owned(),
        token: None,
    };
    let params = session_new_params("/tmp/ws", json!({}), Some(&binding), &[]);
    assert_eq!(
        params["mcpServers"],
        json!([{
            "type": "http",
            "name": "workspace",
            "url": "http://127.0.0.1:8081/sandboxes/env_02/mcp",
            "headers": [],
            "_meta": {"peri.swarmSandbox": true},
        }])
    );
}

// ── S9a4b: `session/load` reattach wire shape ───────────────────────────────

#[test]
fn reattach_loads_the_persisted_id_with_the_same_servers_and_no_new_meta() {
    let binding = SandboxMcpBinding {
        logical_env_id: "env_03".to_owned(),
        sandbox_id: "sbx_0123456789abcdef0123456789abcdef".to_owned(),
        workspace_path: std::path::PathBuf::from("/tmp/sandbox/workspace"),
        mcp_url: "http://127.0.0.1:8081/sandboxes/env_03/mcp".to_owned(),
        token: Some("outer-secret".to_owned()),
    };
    let params = session_load_params(
        "/tmp/ws",
        "0192f0c4-0000-7000-8000-000000000002",
        Some(&binding),
        &declared_tools(),
    );

    assert_eq!(
        params["sessionId"],
        json!("0192f0c4-0000-7000-8000-000000000002"),
        "reattach must reuse the persisted root session id"
    );
    assert_eq!(params["cwd"], json!("/tmp/ws"));
    assert_eq!(
        params.get("_meta"),
        None,
        "session/load has no rootSessionId/instructions meta: identity comes from sessionId"
    );
    let servers = params["mcpServers"].as_array().expect("mcpServers");
    assert_eq!(servers.len(), 3);
    assert_eq!(servers[0]["name"], json!("workspace"));
    assert_eq!(
        servers[0]["_meta"],
        json!({"peri.swarmSandbox": true}),
        "the internal workspace entry is rebuilt identically"
    );
    assert_eq!(servers[1]["name"], json!("deepwiki"));
    assert_eq!(servers[2]["name"], json!("plain"));
}

#[test]
fn reattach_without_servers_keeps_the_load_request_minimal() {
    let params = session_load_params("/tmp/ws", "0192f0c4-0000-7000-8000-000000000002", None, &[]);
    assert_eq!(
        params,
        json!({
            "sessionId": "0192f0c4-0000-7000-8000-000000000002",
            "cwd": "/tmp/ws",
        })
    );
}
