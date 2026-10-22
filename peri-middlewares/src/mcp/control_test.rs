//! Observable control behavior over a real child MCP transport and owned tasks.
use super::*;
use crate::mcp::{
    client::{ControlledMcpService, McpServiceWrapper},
    config::{ConfigSource, McpCachePolicy},
    task_scope::McpTaskOwner,
    McpInitStatus,
};
use serde_json::json;
use std::sync::atomic::AtomicUsize;

#[cfg(not(target_os = "emscripten"))]
#[path = "control_lock_order_test.rs"]
mod lock_order;

fn pool_fixture() -> (McpTaskOwner, Arc<McpClientPool>, tempfile::TempDir) {
    let (owner, spawner) = McpTaskOwner::new();
    let mut pool = McpClientPool::new_empty_with_cache_policy(McpCachePolicy::Disabled);
    pool.task_spawner = spawner;
    let cwd = tempfile::tempdir().unwrap();
    pool.bind_execution_cwd(cwd.path()).unwrap();
    *pool.init_status.write() = McpInitStatus::Ready { total: 0 };
    (owner, Arc::new(pool), cwd)
}
fn request(action: &str, name: &str) -> McpControlRequest {
    serde_json::from_value(json!({"action":action,"sessionId":"s1","serverName":name})).unwrap()
}
fn plain_config() -> super::super::config::McpServerConfig {
    McpRegistrationConfig::Stdio {
        command: "unused".into(),
        args: vec![],
        env: HashMap::new(),
    }
    .into_server_config()
}

#[cfg(not(target_os = "emscripten"))]
fn wire_registration(name: &str) -> McpControlRequest {
    // Legacy negotiation deliberately exercises the same compatibility path as
    // configured servers. A PID response proves restart replaced the child.
    let script = r#"
import json, os, sys
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req:
        continue
    method = req.get('method')
    if method == 'initialize':
        result = {'protocolVersion':'2025-03-26','capabilities':{'tools':{},'resources':{}},'serverInfo':{'name':'control-fixture','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'echo','description':'fixture','inputSchema':{'type':'object','properties':{}}}]}
    elif method == 'tools/call':
        result = {'content':[{'type':'text','text':str(os.getpid())}],'isError':False}
    elif method == 'resources/list':
        result = {'resources':[]}
    elif method == 'resources/templates/list':
        result = {'resourceTemplates':[]}
    elif method == 'ping':
        result = {}
    else:
        print(json.dumps({'jsonrpc':'2.0','id':req['id'],'error':{'code':-32601,'message':'Method not found'}}), flush=True)
        continue
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}), flush=True)
"#;
    McpControlRequest::Register {
        session_id: "s1".into(),
        server_name: name.into(),
        config: McpRegistrationConfig::Stdio {
            command: if cfg!(windows) { "python" } else { "python3" }.into(),
            args: vec!["-u".into(), "-c".into(), script.into()],
            env: HashMap::new(),
        },
    }
}

#[cfg(not(target_os = "emscripten"))]
#[tokio::test]
async fn wire_register_stop_restart_rejects_old_bridge_and_isolates_pools() {
    let (mut owner, pool, _cwd) = pool_fixture();
    let (mut other_owner, other, _other_cwd) = pool_fixture();
    let source = catalog_source(pool.clone(), "s1");
    let initial = source.snapshot();
    let registered = pool
        .clone()
        .control_mcp(wire_registration("docs"))
        .await
        .unwrap();
    assert_eq!(registered.status, McpControlStatus::Connected);
    let first_pid = pool
        .call_instance_tool("docs", "echo", json!({}), None)
        .await
        .unwrap();
    let old_handle = pool.get_client("docs").unwrap();
    let connected = source.snapshot();
    assert!(connected.generation > initial.generation);
    let old_tool = connected
        .tools
        .iter()
        .find(|t| t.mcp_tool_name() == Some("echo"))
        .unwrap()
        .clone();
    assert_eq!(
        other
            .clone()
            .control_mcp(request("stop", "docs"))
            .await
            .unwrap_err()
            .kind,
        "server_not_found"
    );
    assert!(catalog_source(other.clone(), "s1")
        .snapshot()
        .tools
        .is_empty());
    assert_eq!(
        pool.clone()
            .control_mcp(wire_registration("DOCS"))
            .await
            .unwrap_err()
            .kind,
        "name_conflict"
    );

    let stopped = pool
        .clone()
        .control_mcp(request("stop", "docs"))
        .await
        .unwrap();
    assert_eq!(stopped.status, McpControlStatus::Stopped);
    assert!(!old_handle.call_gate.is_open());
    let error = old_tool
        .invoke(json!({}), peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<super::super::ToolCallError>(),
        Some(super::super::ToolCallError::Unavailable { .. })
    ));
    let after_stop = source.snapshot();
    assert!(after_stop.generation > connected.generation);
    assert!(after_stop.tools.is_empty());
    assert!(pool.server_slot("docs").processes.lock().is_empty());
    assert!(pool.spawn_reconnect("docs".into()).is_err());
    assert!(pool.reconnect("docs", None).await.is_err());
    assert!(pool
        .clone()
        .control_mcp(request("stop", "docs"))
        .await
        .is_ok());

    let restarted = pool
        .clone()
        .control_mcp(request("restart", "docs"))
        .await
        .unwrap();
    assert_eq!(restarted.status, McpControlStatus::Connected);
    let next_pid = pool
        .call_instance_tool("docs", "echo", json!({}), None)
        .await
        .unwrap();
    assert_ne!(first_pid, next_pid);
    assert!(!Arc::ptr_eq(&old_handle, &pool.get_client("docs").unwrap()));
    assert!(!old_handle.call_gate.is_open());
    let error = old_tool
        .invoke(json!({}), peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<super::super::ToolCallError>(),
        Some(super::super::ToolCallError::Unavailable { .. })
    ));
    pool.clone()
        .control_mcp(request("stop", "docs"))
        .await
        .unwrap();
    pool.begin_shutdown();
    other.begin_shutdown();
    assert_eq!(
        owner.shutdown().await,
        super::super::task_scope::McpTaskShutdownReport::Complete
    );
    assert_eq!(
        other_owner.shutdown().await,
        super::super::task_scope::McpTaskShutdownReport::Complete
    );
    assert!(pool.shutdown().await.is_complete());
    assert!(other.shutdown().await.is_complete());
}

#[tokio::test]
async fn protected_sources_are_rejected_before_oauth_or_namespace_mutation() {
    let (_owner, pool, _cwd) = pool_fixture();
    for (source, system, reason) in [
        (
            Some(ConfigSource::Builtin {
                instance: "workspace".into(),
            }),
            false,
            "builtin",
        ),
        (
            Some(ConfigSource::WorkspaceRemote),
            false,
            "workspace_remote",
        ),
        (Some(ConfigSource::Acp), false, "acp"),
        (None, true, "system_mcp"),
    ] {
        let mut config = plain_config();
        config.source = source;
        config.system_mcp = Some(system);
        pool.configs.write().insert("protected".into(), config);
        assert_eq!(
            pool.control_descriptor("protected")
                .unavailable_reason
                .as_deref(),
            Some(reason)
        );
        assert_eq!(
            pool.clone()
                .control_mcp(request("stop", "protected"))
                .await
                .unwrap_err()
                .kind,
            "unsupported"
        );
        assert!(!pool
            .server_slot("protected")
            .stopped
            .load(Ordering::Acquire));
    }
    pool.configs
        .write()
        .insert("plugin:tools:docs".into(), plain_config());
    assert_eq!(
        pool.clone()
            .control_mcp(request("stop", "plugin:tools:docs"))
            .await
            .unwrap()
            .status,
        McpControlStatus::Stopped
    );
    let stopped = pool
        .all_server_infos()
        .into_iter()
        .find(|info| info.name == "plugin:tools:docs")
        .unwrap();
    assert_eq!(stopped.status, ClientStatus::Disconnected);
    assert_eq!(stopped.status_label, "disconnected");
    pool.set_stdio_available(false).unwrap();
    let registration = McpControlRequest::Register {
        session_id: "s1".into(),
        server_name: "not-created".into(),
        config: McpRegistrationConfig::Stdio {
            command: "unused".into(),
            args: vec![],
            env: HashMap::new(),
        },
    };
    assert_eq!(
        pool.clone()
            .control_mcp(registration)
            .await
            .unwrap_err()
            .kind,
        "unsupported"
    );
    assert!(!pool.configs.read().contains_key("not-created"));
}

#[tokio::test]
async fn cleanup_failure_stays_owned_and_stop_retry_never_publishes_tools() {
    let (_owner, pool, _cwd) = pool_fixture();
    pool.configs.write().insert("docs".into(), plain_config());
    let count = Arc::new(AtomicUsize::new(0));
    pool.services.lock().insert(
        "docs".into(),
        McpServiceWrapper::Controlled(ControlledMcpService::timing_out(count.clone())),
    );
    for expected in 1..=2 {
        let error = pool
            .clone()
            .control_mcp(request("stop", "docs"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, "shutdown_incomplete");
        assert_eq!(count.load(Ordering::SeqCst), expected);
        assert_eq!(pool.server_slot("docs").services.lock().len(), 1);
        assert!(catalog_source(pool.clone(), "s1")
            .snapshot()
            .tools
            .is_empty());
    }
}

#[tokio::test]
async fn stop_cancels_owned_oauth_wait_and_rejects_late_callback() {
    let (mut owner, pool, _cwd) = pool_fixture();
    pool.configs.write().insert("docs".into(), plain_config());
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow"),
        super::super::client::OAuthStartDisposition::Started
    );
    let slot = pool.server_slot("docs");
    let held = slot.control_operation.lock().await;
    assert_eq!(
        pool.clone()
            .control_mcp(request("stop", "docs"))
            .await
            .unwrap_err()
            .kind,
        "server_busy"
    );
    assert_eq!(pool.active_oauth_flow("docs").as_deref(), Some("flow"));
    drop(held);
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    pool.spawn_background(McpTaskKey::OAuth("flow".into()), async move {
        let _connection = slot.operation.lock().await;
        let _ = entered.send(());
        std::future::pending::<()>().await;
    })
    .unwrap();
    entered_rx.await.unwrap();
    let stopped = peri_time::timeout(
        std::time::Duration::from_secs(1),
        pool.clone().control_mcp(request("stop", "docs")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stopped.status, McpControlStatus::Stopped);
    assert!(pool.active_oauth_flow("docs").is_none());
    let (tx, _rx) = tokio::sync::oneshot::channel();
    assert!(!pool.register_oauth_callback("docs", "flow", tx));
    pool.begin_shutdown();
    assert_eq!(
        owner.shutdown().await,
        super::super::task_scope::McpTaskShutdownReport::Complete
    );
    assert!(pool.shutdown().await.is_complete());
}

#[tokio::test]
async fn register_oauth_without_credentials_returns_authorization_without_callback() {
    let (_owner, pool, _cwd) = pool_fixture();
    let request: McpControlRequest = serde_json::from_value(json!({
        "sessionId":"s1", "serverName":"authdocs", "action":"register",
        "config":{"type":"http","url":"http://127.0.0.1:1/mcp","oauth":{}}
    }))
    .unwrap();
    let response = pool.clone().control_mcp(request).await.unwrap();
    assert_eq!(response.status, McpControlStatus::NeedsAuthorization);
    assert!(pool.active_oauth_flow("authdocs").is_none());
    assert!(catalog_source(pool.clone(), "s1")
        .snapshot()
        .tools
        .is_empty());
}

#[cfg(not(target_os = "emscripten"))]
#[tokio::test]
async fn early_registration_failure_is_queryable_and_can_be_stopped() {
    // Ready configuration with no execution directory fails before a transport
    // exists. The reserved registration must still expose its failure in list.
    let pool = Arc::new(McpClientPool::new_empty_with_cache_policy(
        McpCachePolicy::Disabled,
    ));
    *pool.init_status.write() = McpInitStatus::Ready { total: 0 };
    let registration = McpControlRequest::Register {
        session_id: "s1".into(),
        server_name: "docs".into(),
        config: McpRegistrationConfig::Stdio {
            command: "must-not-execute".into(),
            args: vec![],
            env: HashMap::new(),
        },
    };
    let error = pool.clone().control_mcp(registration).await.unwrap_err();
    assert_eq!(error.kind, "connection_failed");
    let failed = pool
        .all_server_infos()
        .into_iter()
        .find(|info| info.name == "docs")
        .unwrap();
    assert!(matches!(failed.status, ClientStatus::Failed(_)));
    assert_eq!(failed.status_label, "failed");
    assert!(failed.error_summary.is_some());
    let stopped = pool
        .clone()
        .control_mcp(request("stop", "docs"))
        .await
        .unwrap();
    assert_eq!(stopped.status, McpControlStatus::Stopped);
    let info = pool
        .all_server_infos()
        .into_iter()
        .find(|info| info.name == "docs")
        .unwrap();
    assert_eq!(info.status, ClientStatus::Disconnected);
    assert_eq!(info.status_label, "disconnected");
    assert!(pool.shutdown().await.is_complete());
}
