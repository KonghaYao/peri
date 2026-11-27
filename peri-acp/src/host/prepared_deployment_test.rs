//! Deployment source restrictions across a frozen session restore.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serial_test::serial;
use tempfile::TempDir;

use super::{
    canonical_workspace_dir, host_with_plugin_face, poison_plugin_manifest, seed_enabled_plugin,
    write_plugin_manifest, PreparedHomeGuard, PreparedSessionInputs,
};
use crate::host::{assemble, requests, workspace};

#[tokio::test]
#[serial]
async fn deployment_plugin_absence_stays_closed_across_restore() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = PreparedHomeGuard::set(&home);
    let plugin_dir = seed_enabled_plugin(&home);
    let cwd = canonical_workspace_dir(&tmp, "workspace");
    let mut host = host_with_plugin_face(&tmp, &cwd, false).await;

    let open = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert!(open.plugins().data.is_some());
    poison_plugin_manifest(&plugin_dir);

    host.workspace_assembly
        .as_mut()
        .unwrap()
        .capabilities
        .plugins = false;
    let closed = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    let restore_on_restricted_host = PreparedSessionInputs::prepare_restore(
        &host,
        &cwd,
        open.frozen_encoded.as_deref().unwrap(),
    )
    .unwrap();
    for inputs in [closed.plugins(), restore_on_restricted_host.plugins()] {
        assert!(inputs.data.is_none());
        assert!(inputs.skill_roots.is_empty());
        assert!(
            !inputs.discovery_failed,
            "closed deployment must not discover plugins"
        );
    }

    host.workspace_assembly
        .as_mut()
        .unwrap()
        .capabilities
        .plugins = true;
    let restored = PreparedSessionInputs::prepare_restore(
        &host,
        &cwd,
        closed.frozen_encoded.as_deref().unwrap(),
    )
    .unwrap();
    assert!(restored.plugins().data.is_none());
    assert!(
        !restored.plugins().discovery_failed,
        "frozen closure must prevent discovery"
    );
    assert!(restored
        .frozen
        .as_ref()
        .unwrap()
        .meta_harness()
        .disabled_middlewares
        .contains(peri_middlewares::assembly::PLUGIN_FACE_CLOSED_KEY));
}

#[tokio::test]
#[serial]
async fn restored_deployment_source_limits_keep_plugin_hooks_and_builtin_mcp() {
    use peri_acp_types::mcp_control::{McpControlRequest, McpRegistrationConfig};

    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = PreparedHomeGuard::set(&home);
    let plugin_dir = seed_enabled_plugin(&home);
    write_plugin_manifest(&plugin_dir, "{}");
    let hook =
        r#"{"hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"echo fixture"}]}]}}"#;
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(plugin_dir.join("hooks/hooks.json"), hook).unwrap();
    let cwd = canonical_workspace_dir(&tmp, "workspace");
    std::fs::create_dir_all(Path::new(&cwd).join(".claude")).unwrap();
    std::fs::write(Path::new(&cwd).join(".claude/settings.json"), hook).unwrap();
    let mut host = host_with_plugin_face(&tmp, &cwd, false).await;
    host.config_source = Arc::new(
        crate::provider::ConfigSource::load_at(
            Path::new(&cwd),
            tmp.path().join("test_config.json"),
        )
        .unwrap(),
    );
    let workspace = host
        .session_resources
        .resolve_workspace(Path::new(&cwd))
        .await
        .unwrap();

    let full = assemble::HostCapabilities::default();
    let restricted = assemble::HostCapabilities {
        stdio_mcp: false,
        settings_hooks: false,
        ..full
    };
    for (original, reopening) in [(restricted, full), (full, restricted)] {
        host.workspace_assembly.as_mut().unwrap().capabilities = original;
        let inputs = PreparedSessionInputs::prepare_new_deferred(&host, &cwd).unwrap();
        let mut sessions = HashMap::new();
        let created = requests::session_lifecycle::new_session_from_prepared(
            &host,
            &workspace,
            inputs,
            &mut sessions,
        )
        .await
        .unwrap();
        let id = created["sessionId"].as_str().unwrap().to_owned();
        let snapshot = host
            .session_resources
            .load_session_snapshot(&id)
            .await
            .unwrap();
        let persisted = match snapshot.frozen {
            peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes.into_string(),
            other => panic!("expected a persisted frozen snapshot: {other:?}"),
        };
        let original_env = sessions.remove(&id).unwrap().environment.unwrap();
        assert!(!original_env.cfg.plugin_hooks_only.is_empty());
        assert_eq!(
            original_env
                .cfg
                .hook_groups
                .iter()
                .flatten()
                .any(|hook| hook.plugin_name == "project-settings.json"),
            original.settings_hooks
        );
        assert!(original_env.shutdown().await);

        host.workspace_assembly.as_mut().unwrap().capabilities = reopening;
        let prepared = PreparedSessionInputs::prepare_restore(&host, &cwd, &persisted).unwrap();
        let env = workspace::SessionEnvironment::assemble_prepared(&host, &prepared, &id)
            .await
            .unwrap()
            .unwrap();
        assert!(!env.cfg.plugin_hooks_only.is_empty());
        assert_eq!(
            env.cfg.hook_groups.iter().flatten().count(),
            env.cfg.plugin_hooks_only.len()
        );
        assert!(env.mcp_control_enabled);

        env.activate();
        env.read_meta_docs(&Default::default()).await.unwrap();
        let pool = env.cfg.mcp_pool.as_ref().unwrap();
        assert!(
            matches!(
                pool.builtin_workspace_state(),
                peri_acp_types::ports::McpBuiltinWorkspaceState::Connected
            ),
            "pool snapshot: {}",
            pool.snapshot()
        );
        let rejected = pool
            .clone()
            .control_mcp(McpControlRequest::Register {
                session_id: id.to_owned(),
                server_name: "stdio-blocked-fixture".to_owned(),
                config: McpRegistrationConfig::Stdio {
                    command: "must-not-be-launched".to_owned(),
                    args: Vec::new(),
                    env: HashMap::new(),
                },
            })
            .await
            .expect_err("restored source limit must reject stdio registration");
        assert_eq!(rejected.kind, "unsupported");
        assert!(env.shutdown().await);
    }
}
