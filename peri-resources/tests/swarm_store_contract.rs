//! `SwarmStorePort` 的公开面契约：本机 store 明确返回 `Unsupported`，不回退本机 SQLite。
//!
//! 远端端口需要真实 Loom 或 crate 内的远端夹具，只在 `peri-resources` 单测里验证；这里只守
//! `Resources::swarm_store()` 的公开语义：访问器总是可用，但本机部署的每一次调用都以
//! `Unsupported` 失败，且失败不是「空结果」。

use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_resources::{
    Resources, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmSessionUpdate,
};

fn session_row() -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: "agent-local".to_owned(),
        root_session_id: "root-local".to_owned(),
        actor_key: "actor-local".to_owned(),
        cwd: "/work".to_owned(),
        state: "idle".to_owned(),
        model: None,
        instructions: None,
        mcp_json: None,
        environment_json: r#"{"type":"none","id":"agent-local"}"#.to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00Z".to_owned(),
        last_item_seq: 0,
    }
}

fn item_row() -> SwarmItemRow {
    SwarmItemRow {
        logical_id: "agent-local".to_owned(),
        seq: 1,
        item_id: "itim_local".to_owned(),
        kind: "message".to_owned(),
        source_json: r#"{"agentId":"root"}"#.to_owned(),
        payload_json: r#"{"text":"hello"}"#.to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        truncated: false,
    }
}

fn input_row() -> SwarmInputRow {
    SwarmInputRow {
        logical_id: "agent-local".to_owned(),
        input_id: "input-local".to_owned(),
        fingerprint: "sha256:test".to_owned(),
        state: "accepted".to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn binding_row() -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: "env-local".to_owned(),
        logical_id: "agent-local".to_owned(),
        sandbox_id: "sandbox-local".to_owned(),
        workspace_path: "/work".to_owned(),
        sandbox_root: "/sandboxes".to_owned(),
        settings_profile: None,
        token_file: None,
        last_run_id: None,
        last_used_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

#[tokio::test]
async fn local_store_reports_the_swarm_port_as_unsupported() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let resources = Resources::open_with(Some(path)).await.unwrap();
    let store = resources.swarm_store();

    let failures = vec![
        store.swarm_upsert_session(session_row()).await.err(),
        store.swarm_get_session("agent-local").await.err(),
        store
            .swarm_update_session(SwarmSessionUpdate {
                logical_id: "agent-local".to_owned(),
                state: "running".to_owned(),
                last_item_seq: 1,
                last_activity_at: "2026-01-01T00:00:00Z".to_owned(),
            })
            .await
            .err(),
        store.swarm_append_item(item_row()).await.err(),
        store.swarm_items_after("agent-local", 0, 10).await.err(),
        store
            .swarm_items_before("agent-local", None, 10)
            .await
            .err(),
        store.swarm_last_seq("agent-local").await.err(),
        store.swarm_put_input(input_row()).await.err(),
        store
            .swarm_get_input("agent-local", "input-local")
            .await
            .err(),
        store.swarm_put_binding(binding_row()).await.err(),
        store.swarm_get_binding("env-local").await.err(),
    ];
    for (index, failure) in failures.into_iter().enumerate() {
        let error = failure.unwrap_or_else(|| panic!("调用 {index} 必须失败，不能回退本机库"));
        assert!(
            matches!(error.kind(), SessionResourceErrorKind::Unsupported),
            "调用 {index} 必须是显式 unsupported: {error:?}"
        );
    }
}
