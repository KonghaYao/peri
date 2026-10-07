use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use serde_json::{json, Value};

use super::{
    ConfigurationError, ConfigurationField, ConfigurationInputs, ConfigurationScope,
    ConfigurationSnapshot, ConfigurationSystem,
};
use crate::{app::PeriConfig, mcp::McpConfigFile, source::ConfigurationSource};

#[derive(Default)]
struct FakeState {
    inputs: HashMap<ConfigurationScope, ConfigurationInputs>,
    collect_calls: usize,
    write_calls: usize,
    reject_next_write: bool,
    fail_writes: bool,
    fail_collects: bool,
    writes: Vec<(PathBuf, Option<String>, String)>,
}

#[derive(Default)]
struct FakeSource {
    state: Mutex<FakeState>,
}

impl FakeSource {
    fn set_inputs(&self, scope: &ConfigurationScope, inputs: ConfigurationInputs) {
        self.state
            .lock()
            .unwrap()
            .inputs
            .insert(scope.clone(), inputs);
    }

    fn mutate_inputs(
        &self,
        scope: &ConfigurationScope,
        mutate: impl FnOnce(&mut ConfigurationInputs),
    ) {
        mutate(self.state.lock().unwrap().inputs.get_mut(scope).unwrap());
    }

    fn reject_next_write(&self) {
        self.state.lock().unwrap().reject_next_write = true;
    }

    fn fail_writes(&self) {
        self.state.lock().unwrap().fail_writes = true;
    }

    fn writes(&self) -> Vec<(PathBuf, Option<String>, String)> {
        self.state.lock().unwrap().writes.clone()
    }

    fn io_counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.collect_calls, state.write_calls)
    }
}

impl ConfigurationSource for FakeSource {
    fn collect(&self, scope: &ConfigurationScope) -> io::Result<ConfigurationInputs> {
        let mut state = self.state.lock().unwrap();
        state.collect_calls += 1;
        if state.fail_collects {
            return Err(io::Error::other("collect failed"));
        }
        state
            .inputs
            .get(scope)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "scope missing"))
    }

    fn write_if_unchanged(
        &self,
        path: &Path,
        expected: Option<&str>,
        content: &str,
    ) -> io::Result<bool> {
        let mut state = self.state.lock().unwrap();
        state.write_calls += 1;
        if state.fail_writes {
            return Err(io::Error::other("write failed"));
        }
        if state.reject_next_write {
            state.reject_next_write = false;
            return Ok(false);
        }
        let expected = expected.map(str::to_owned);
        let matching_scopes: Vec<_> = state
            .inputs
            .keys()
            .filter(|scope| {
                path == scope.global_settings
                    || path == scope.cwd.join(".peri/settings.json")
                    || path == scope.cwd.join(".mcp.json")
            })
            .cloned()
            .collect();
        if matching_scopes.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "path missing"));
        }
        let matches_expected = matching_scopes.iter().all(|scope| {
            let inputs = &state.inputs[scope];
            let current = if path == scope.global_settings {
                &inputs.global
            } else if path == scope.cwd.join(".peri/settings.json") {
                &inputs.workspace
            } else {
                &inputs.project
            };
            *current == expected
        });
        if !matches_expected {
            return Ok(false);
        }
        for scope in &matching_scopes {
            let inputs = state.inputs.get_mut(scope).unwrap();
            let target = if path == scope.global_settings {
                &mut inputs.global
            } else if path == scope.cwd.join(".peri/settings.json") {
                &mut inputs.workspace
            } else {
                &mut inputs.project
            };
            *target = Some(content.to_owned());
        }
        state
            .writes
            .push((path.to_owned(), expected, content.to_owned()));
        Ok(true)
    }
}

fn make_scope(root: &Path, cwd_name: &str) -> ConfigurationScope {
    ConfigurationScope::new(root.join(cwd_name), root.join("global/settings.json")).unwrap()
}

fn make_inputs(
    global: Option<&str>,
    workspace: Option<&str>,
    project: Option<&str>,
) -> ConfigurationInputs {
    ConfigurationInputs {
        global: global.map(str::to_owned),
        workspace: workspace.map(str::to_owned),
        project: project.map(str::to_owned),
        environment: Default::default(),
    }
}

fn make_system(
    scope: &ConfigurationScope,
    inputs: ConfigurationInputs,
) -> (Arc<FakeSource>, ConfigurationSystem) {
    let source = Arc::new(FakeSource::default());
    source.set_inputs(scope, inputs);
    let system = ConfigurationSystem::new(source.clone());
    (source, system)
}

fn make_default_mcp_server() -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    }
}

#[test]
fn provider_selection_uses_scoped_configuration_environment() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let settings = r#"{"config":{"active_alias":"opus","providers":[{"id":"first","type":"openai","apiKey":"first-key","models":{"opus":"first-model"}},{"id":"second","type":"openai","apiKey":"second-key","models":{"sonnet":"second-model"}}]}}"#;
    let mut inputs = make_inputs(Some(settings), None, None);
    inputs
        .environment
        .insert("MODEL_PROVIDER".into(), "second".into());
    inputs
        .environment
        .insert("MODEL_TYPE".into(), "sonnet".into());
    let selected = ConfigurationSnapshot::resolve(scope.clone(), inputs.clone()).unwrap();
    assert!(
        matches!(selected.provider(), Some(crate::provider::ResolvedProvider::OpenAi { model, api_key, .. }) if model == "second-model" && api_key == "second-key")
    );

    inputs.environment.remove("MODEL_TYPE");
    let incomplete = ConfigurationSnapshot::resolve(scope, inputs).unwrap();
    assert!(incomplete.provider().is_none());
}

#[test]
fn same_scope_inputs_have_deterministic_revision_and_scope_is_part_of_revision() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project-a");
    let inputs = make_inputs(Some(r#"{"config":{"active_alias":"sonnet"}}"#), None, None);
    let first = ConfigurationSnapshot::resolve(scope.clone(), inputs.clone()).unwrap();
    let second = ConfigurationSnapshot::resolve(scope.clone(), inputs.clone()).unwrap();
    assert_eq!(first.revision(), second.revision());

    let other_scope = make_scope(temp.path(), "project-b");
    let other = ConfigurationSnapshot::resolve(other_scope, inputs).unwrap();
    assert_ne!(first.revision(), other.revision());
}

#[test]
fn different_projects_resolve_isolated_project_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let first_scope = make_scope(temp.path(), "project-a");
    let second_scope = make_scope(temp.path(), "project-b");
    let first = ConfigurationSnapshot::resolve(
        first_scope,
        make_inputs(
            None,
            None,
            Some(r#"{"mcpServers":{"one":{"command":"one"}}}"#),
        ),
    )
    .unwrap();
    let second = ConfigurationSnapshot::resolve(
        second_scope,
        make_inputs(
            None,
            None,
            Some(r#"{"mcpServers":{"two":{"command":"two"}}}"#),
        ),
    )
    .unwrap();
    assert!(first.mcp().mcp_servers.contains_key("one"));
    assert!(!first.mcp().mcp_servers.contains_key("two"));
    assert!(second.mcp().mcp_servers.contains_key("two"));
    assert!(!second.mcp().mcp_servers.contains_key("one"));
}

#[test]
fn workspace_profiles_replace_as_units_and_meta_harness_merges_keys() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let first_key = peri_acp_types::meta_harness::SECTION_IDS[0];
    let second_key = peri_acp_types::meta_harness::SECTION_IDS[1];
    let global = format!(
        r#"{{"config":{{"profiles":{{"sonnet":{{"provider":"global-provider","model":"global-model","effort":"max"}}}},"meta_harness":{{"{first_key}":true,"{second_key}":false}}}}}}"#
    );
    let workspace = format!(
        r#"{{"config":{{"profiles":{{"sonnet":{{"provider":"workspace-provider"}}}},"meta_harness":{{"{first_key}":false}}}}}}"#
    );
    let snapshot =
        ConfigurationSnapshot::resolve(scope, make_inputs(Some(&global), Some(&workspace), None))
            .unwrap();
    let sonnet = snapshot.settings().config.profiles.sonnet.clone();
    assert_eq!(sonnet.provider, "workspace-provider");
    assert_eq!(sonnet.model, None);
    assert_eq!(sonnet.effort, "xhigh");
    assert!(!snapshot.settings().config.meta_harness.as_ref().unwrap()[first_key]);
    assert!(!snapshot.settings().config.meta_harness.as_ref().unwrap()[second_key]);
}

#[test]
fn cache_disabled_in_any_scope_wins() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let cases = [
        (
            Some(r#"{"mcpCache":false}"#),
            Some(r#"{"mcpCache":true}"#),
            "on",
        ),
        (
            Some(r#"{"mcpCache":true}"#),
            Some(r#"{"mcpCache":false}"#),
            "on",
        ),
        (
            Some(r#"{"mcpCache":true}"#),
            Some(r#"{"mcpCache":true}"#),
            "off",
        ),
    ];
    for (global, project, environment_cache) in cases {
        let mut inputs = make_inputs(global, None, project);
        inputs
            .environment
            .insert("PERI_MCP_CACHE".into(), environment_cache.into());
        let snapshot = ConfigurationSnapshot::resolve(scope.clone(), inputs).unwrap();
        assert_eq!(
            snapshot.cache_policy(),
            crate::mcp::McpCachePolicy::Disabled
        );
    }
}

#[test]
fn resolved_snapshot_is_frozen_after_source_changes_and_reload() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(
        &scope,
        make_inputs(Some(r#"{"config":{"active_alias":"sonnet"}}"#), None, None),
    );
    let before = system.resolve(scope.clone()).unwrap();
    source.mutate_inputs(&scope, |inputs| {
        inputs.global = Some(r#"{"config":{"active_alias":"haiku"}}"#.into());
    });
    assert_eq!(before.settings().config.active_alias, "sonnet");
    let after = system.resolve(scope.clone()).unwrap();
    assert_eq!(after.settings().config.active_alias, "haiku");
    assert_eq!(before.settings().config.active_alias, "sonnet");
    assert!(!Arc::ptr_eq(&before, &after));
}

#[test]
fn failed_resolve_keeps_the_current_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let current = system.resolve(scope.clone()).unwrap();
    source.mutate_inputs(&scope, |inputs| inputs.global = Some("not-json".into()));
    assert!(matches!(
        system.resolve(scope.clone()),
        Err(ConfigurationError::InvalidInput { .. })
    ));
    assert!(Arc::ptr_eq(&current, &system.current(&scope).unwrap()));
}

#[test]
fn update_rejects_stale_registry_revision_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let old = system.resolve(scope.clone()).unwrap();
    source.mutate_inputs(&scope, |inputs| {
        inputs.global = Some(r#"{"config":{"language":"en"}}"#.into())
    });
    let current = system.resolve(scope.clone()).unwrap();
    let err = system
        .update(&scope, old.revision(), current.settings())
        .unwrap_err();
    assert!(matches!(err, ConfigurationError::Conflict));
    assert!(source.writes().is_empty());
}

#[test]
fn update_rejects_external_disk_and_environment_changes_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(Some(r#"{"config":{}}"#), None, None));
    let snapshot = system.resolve(scope.clone()).unwrap();
    source.mutate_inputs(&scope, |inputs| {
        inputs.global = Some(r#"{"config":{"language":"fr"}}"#.into())
    });
    let err = system
        .update(&scope, snapshot.revision(), snapshot.settings())
        .unwrap_err();
    assert!(matches!(err, ConfigurationError::Conflict));
    assert!(source.writes().is_empty());

    let next = system.resolve(scope.clone()).unwrap();
    source.mutate_inputs(&scope, |inputs| {
        inputs
            .environment
            .insert("LANGFUSE_SECRET_KEY".into(), "changed-secret".into());
    });
    let err = system
        .update(&scope, next.revision(), next.settings())
        .unwrap_err();
    assert!(matches!(err, ConfigurationError::Conflict));
    assert!(source.writes().is_empty());
}

#[test]
fn failed_cas_or_io_write_does_not_publish_next_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let current = system.resolve(scope.clone()).unwrap();
    let mut updated: PeriConfig = current.settings().clone();
    updated.config.language = Some("en".into());

    source.reject_next_write();
    assert!(matches!(
        system.update(&scope, current.revision(), &updated),
        Err(ConfigurationError::Conflict)
    ));
    assert!(Arc::ptr_eq(&current, &system.current(&scope).unwrap()));

    source.fail_writes();
    assert!(matches!(
        system.update(&scope, current.revision(), &updated),
        Err(ConfigurationError::InputUnavailable(_))
    ));
    assert!(Arc::ptr_eq(&current, &system.current(&scope).unwrap()));
    assert!(source.writes().is_empty());
}

#[test]
fn invalid_mcp_update_is_rejected_before_write() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let current = system.resolve(scope.clone()).unwrap();
    let mut server = make_default_mcp_server();
    server.system_mcp_tools = Some(Vec::new());
    let invalid = McpConfigFile {
        mcp_servers: HashMap::from([("invalid".into(), server)]),
        mcp_cache: None,
    };
    assert!(matches!(
        system.update_mcp(&scope, current.revision(), &invalid),
        Err(ConfigurationError::InvalidInput { .. })
    ));
    assert!(source.writes().is_empty());
    assert_eq!(source.io_counts().1, 0);
    assert!(Arc::ptr_eq(&current, &system.current(&scope).unwrap()));
}

#[test]
fn mcp_with_plugins_uses_frozen_sources_without_io() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"mcpServers":{"global":{"command":"frozen-global"},"shared":{"command":"global-shared"}}}"#;
    let project = r#"{"mcpServers":{"project":{"command":"frozen-project"},"shared":{"command":"project-shared"}}}"#;
    let mut inputs = make_inputs(Some(global), None, Some(project));
    inputs
        .environment
        .insert("PERI_MCP_CACHE".into(), "off".into());
    let (source, system) = make_system(&scope, inputs);
    let snapshot = system.resolve(scope.clone()).unwrap();
    let revision = snapshot.revision();
    let mut plugin = make_default_mcp_server();
    plugin.command = Some("frozen-plugin".into());
    let mut duplicate = make_default_mcp_server();
    duplicate.command = Some("frozen-global".into());
    let plugins = HashMap::from([
        ("plugin:sample:unique".into(), plugin.clone()),
        ("shared".into(), plugin),
        ("plugin:sample:duplicate".into(), duplicate),
    ]);
    source.mutate_inputs(&scope, |inputs| {
        inputs.global = Some("invalid-new-global".into());
        inputs.project = Some("invalid-new-project".into());
        inputs
            .environment
            .insert("PERI_MCP_CACHE".into(), "on".into());
    });
    {
        let mut state = source.state.lock().unwrap();
        state.fail_collects = true;
        state.fail_writes = true;
    }

    let mut projection = snapshot.mcp_with_plugins(&plugins).unwrap();
    assert_eq!(projection.mcp_cache, Some(false));
    assert_eq!(
        projection.mcp_servers["global"].command.as_deref(),
        Some("frozen-global")
    );
    assert_eq!(
        projection.mcp_servers["global"].source,
        Some(ConfigSource::Global(scope.global_settings.clone()))
    );
    assert_eq!(
        projection.mcp_servers["project"].command.as_deref(),
        Some("frozen-project")
    );
    assert_eq!(
        projection.mcp_servers["project"].source,
        Some(ConfigSource::Project(scope.cwd.join(".mcp.json")))
    );
    assert_eq!(
        projection.mcp_servers["shared"].command.as_deref(),
        Some("project-shared")
    );
    assert_eq!(
        projection.mcp_servers["plugin:sample:unique"].source,
        Some(ConfigSource::Plugin)
    );
    assert!(!projection
        .mcp_servers
        .contains_key("plugin:sample:duplicate"));
    projection.mcp_servers.get_mut("global").unwrap().command = Some("projection-only".into());
    let repeated = snapshot.mcp_with_plugins(&plugins).unwrap();
    assert_eq!(
        repeated.mcp_servers["global"].command.as_deref(),
        Some("frozen-global")
    );
    assert!(!snapshot
        .mcp()
        .mcp_servers
        .contains_key("plugin:sample:unique"));
    assert_eq!(snapshot.revision(), revision);
    assert_eq!(source.io_counts(), (1, 0));
    assert!(Arc::ptr_eq(&snapshot, &system.current(&scope).unwrap()));
}

#[test]
fn settings_update_preserves_root_siblings_and_schema() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"$schema":"https://example.test/schema","config":{"active_alias":"sonnet"},"mcpServers":{"root":{"command":"echo"}},"langfuse":{"trace_sampling":0.4},"extension":{"keep":true}}"#;
    let (source, system) = make_system(&scope, make_inputs(Some(global), None, None));
    let current = system.resolve(scope.clone()).unwrap();
    let mut updated = current.settings().clone();
    updated.config.active_alias = "haiku".into();
    let next = system.update(&scope, current.revision(), &updated).unwrap();
    let (_, _, content) = source.writes().pop().unwrap();
    let document: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(document["$schema"], "https://example.test/schema");
    assert_eq!(document["mcpServers"]["root"]["command"], "echo");
    assert_eq!(document["langfuse"]["trace_sampling"], 0.4);
    assert_eq!(document["extension"]["keep"], true);
    assert_eq!(next.settings().config.active_alias, "haiku");
}

#[test]
fn workspace_settings_write_only_relative_diff_without_global_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"config":{"active_alias":"sonnet","providers":[{"id":"global","type":"openai","apiKey":"sk-global-secret"}],"language":"en"}}"#;
    let workspace =
        r#"{"$schema":"workspace-schema","config":{"active_alias":"haiku"},"extension":"keep"}"#;
    let (source, system) = make_system(&scope, make_inputs(Some(global), Some(workspace), None));
    let current = system.resolve(scope.clone()).unwrap();
    let mut updated = current.settings().clone();
    updated.config.active_alias = "opus".into();
    let next = system.update(&scope, current.revision(), &updated).unwrap();
    let (path, _, content) = source.writes().pop().unwrap();
    assert_eq!(path, scope.cwd.join(".peri/settings.json"));
    assert!(!content.contains("sk-global-secret"));
    let document: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(document["$schema"], "workspace-schema");
    assert_eq!(document["extension"], "keep");
    assert_eq!(document["config"]["active_alias"], "opus");
    assert!(document["config"].get("providers").is_none());
    assert_eq!(next.settings().config.providers.len(), 1);
}

#[test]
fn mcp_update_preserves_top_level_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let project = r#"{"$schema":"mcp-schema","mcpServers":{"old":{"command":"old"}},"extension":{"keep":true},"owner":"team"}"#;
    let (source, system) = make_system(&scope, make_inputs(None, None, Some(project)));
    let current = system.resolve(scope.clone()).unwrap();
    let config: McpConfigFile = serde_json::from_value(json!({
        "mcpServers": {"new": {"command": "new"}},
        "mcpCache": false
    }))
    .unwrap();
    system
        .update_mcp(&scope, current.revision(), &config)
        .unwrap();
    let document: Value = serde_json::from_str(&source.writes().pop().unwrap().2).unwrap();
    assert_eq!(document["$schema"], "mcp-schema");
    assert_eq!(document["extension"]["keep"], true);
    assert_eq!(document["owner"], "team");
    assert_eq!(document["mcpServers"]["new"]["command"], "new");
    assert!(document["mcpServers"].get("old").is_none());
    assert_eq!(document["mcpCache"], false);
}

#[test]
fn global_update_invalidates_other_scopes_sharing_global_settings() {
    let temp = tempfile::tempdir().unwrap();
    let first_scope = make_scope(temp.path(), "project-a");
    let second_scope = make_scope(temp.path(), "project-b");
    let source = Arc::new(FakeSource::default());
    for scope in [&first_scope, &second_scope] {
        source.set_inputs(scope, make_inputs(None, None, None));
    }
    let system = ConfigurationSystem::new(source);
    let first = system.resolve(first_scope.clone()).unwrap();
    system.resolve(second_scope.clone()).unwrap();
    let mut settings = first.settings().clone();
    settings.config.language = Some("en".into());
    let next = system
        .update(&first_scope, first.revision(), &settings)
        .unwrap();
    assert!(system.current(&second_scope).is_none());
    assert!(Arc::ptr_eq(&next, &system.current(&first_scope).unwrap()));
    assert_ne!(first.revision(), next.revision());
}

#[test]
fn settings_update_preserves_nested_mcp_domain_when_request_only_has_provider() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = json!({
        "config": {
            "mcpServers": {"nested": {"command": "original"}},
            "mcpCache": false
        }
    });
    let (source, system) = make_system(&scope, make_inputs(Some(&global.to_string()), None, None));
    let current = system.resolve(scope.clone()).unwrap();
    let request: PeriConfig = serde_json::from_value(json!({
        "config": {"providers": [{"id": "new", "type": "openai", "apiKey": "new-key"}]}
    }))
    .unwrap();
    let next = system.update(&scope, current.revision(), &request).unwrap();
    let writes = source.writes();
    assert_eq!(writes.len(), 1);
    let document: Value = serde_json::from_str(&writes[0].2).unwrap();
    assert_eq!(
        document["config"]["mcpServers"],
        global["config"]["mcpServers"]
    );
    assert_eq!(document["config"]["mcpCache"], false);
    assert_eq!(
        next.mcp().mcp_servers["nested"].command.as_deref(),
        Some("original")
    );
    assert_eq!(next.mcp().mcp_cache, Some(false));
    assert_eq!(next.settings().config.providers[0].id, "new");
}

#[test]
fn workspace_settings_update_preserves_local_mcp_fields_and_schema() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"$schema":"global-schema","config":{}}"#;
    let workspace = json!({
        "$schema": "local-schema",
        "config": {
            "mcpServers": {"local": {"command": "local-command"}},
            "mcpCache": false
        },
        "extension": "keep"
    });
    let (source, system) = make_system(
        &scope,
        make_inputs(Some(global), Some(&workspace.to_string()), None),
    );
    let current = system.resolve(scope.clone()).unwrap();
    let request: PeriConfig = serde_json::from_value(json!({
        "$schema": "global-schema",
        "config": {"providers": [{"id": "new", "type": "openai", "apiKey": "new-key"}]}
    }))
    .unwrap();
    system.update(&scope, current.revision(), &request).unwrap();
    let writes = source.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, scope.cwd.join(".peri/settings.json"));
    let document: Value = serde_json::from_str(&writes[0].2).unwrap();
    assert_eq!(document["$schema"], "local-schema");
    assert_eq!(
        document["config"]["mcpServers"],
        workspace["config"]["mcpServers"]
    );
    assert_eq!(document["config"]["mcpCache"], false);
    assert_eq!(document["extension"], "keep");
}

#[test]
fn workspace_settings_update_cannot_import_global_mcp_fields_or_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"config":{"providers":[{"id":"global","type":"openai","apiKey":"global-provider-secret"}],"mcpServers":{"global":{"command":"global-command","env":{"TOKEN":"global-mcp-secret"}}},"mcpCache":false}}"#;
    let (source, system) = make_system(
        &scope,
        make_inputs(Some(global), Some(r#"{"config":{}}"#), None),
    );
    let current = system.resolve(scope.clone()).unwrap();
    let mut request = current.settings().clone();
    request.config.language = Some("fr".into());
    request.config.extra.insert("mcpCache".into(), json!(true));
    request.config.extra.insert(
        "mcpServers".into(),
        json!({
            "imported": {"command": "imported-command", "env": {"TOKEN": "global-mcp-secret"}}
        }),
    );
    let next = system.update(&scope, current.revision(), &request).unwrap();
    let writes = source.writes();
    assert_eq!(writes.len(), 1);
    let document: Value = serde_json::from_str(&writes[0].2).unwrap();
    assert!(document["config"].get("mcpServers").is_none());
    assert!(document["config"].get("mcpCache").is_none());
    assert!(document["config"].get("providers").is_none());
    assert!(!writes[0].2.contains("global-provider-secret"));
    assert!(!writes[0].2.contains("global-mcp-secret"));
    assert_eq!(next.mcp().mcp_cache, Some(false));
    assert!(next.mcp().mcp_servers.contains_key("global"));
    assert!(!next.mcp().mcp_servers.contains_key("imported"));
}

#[test]
fn mcp_update_rejects_foreign_server_sources_without_writing_or_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let current = system.resolve(scope.clone()).unwrap();
    let foreign_sources = [
        ConfigSource::Global(scope.global_settings.clone()),
        ConfigSource::Plugin,
        ConfigSource::Builtin {
            instance: "workspace".into(),
        },
        ConfigSource::Acp,
        ConfigSource::Project(temp.path().join("other-project/.mcp.json")),
    ];
    for foreign_source in foreign_sources {
        let mut server = make_default_mcp_server();
        server.command = Some("foreign-command".into());
        server.env = Some(HashMap::from([("TOKEN".into(), "foreign-secret".into())]));
        server.source = Some(foreign_source);
        let config = McpConfigFile {
            mcp_servers: HashMap::from([("foreign".into(), server)]),
            mcp_cache: None,
        };
        assert!(matches!(
            system.update_mcp(&scope, current.revision(), &config),
            Err(ConfigurationError::InvalidInput { .. })
        ));
        assert_eq!(source.io_counts().1, 0);
        assert!(source.writes().is_empty());
        assert!(Arc::ptr_eq(&current, &system.current(&scope).unwrap()));
    }
}

#[test]
fn mcp_update_accepts_unattributed_and_current_project_servers() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let current = system.resolve(scope.clone()).unwrap();
    let mut unattributed = make_default_mcp_server();
    unattributed.command = Some("unattributed-command".into());
    let mut local = make_default_mcp_server();
    local.command = Some("local-command".into());
    local.source = Some(ConfigSource::Project(scope.cwd.join(".mcp.json")));
    let config = McpConfigFile {
        mcp_servers: HashMap::from([("new".into(), unattributed), ("local".into(), local)]),
        mcp_cache: None,
    };
    let next = system
        .update_mcp(&scope, current.revision(), &config)
        .unwrap();
    let writes = source.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, scope.cwd.join(".mcp.json"));
    let document: Value = serde_json::from_str(&writes[0].2).unwrap();
    assert_eq!(
        document["mcpServers"]["new"]["command"],
        "unattributed-command"
    );
    assert_eq!(document["mcpServers"]["local"]["command"], "local-command");
    for server in next.mcp().mcp_servers.values() {
        assert_eq!(
            server.source,
            Some(ConfigSource::Project(scope.cwd.join(".mcp.json")))
        );
    }
}

#[test]
fn explanations_omit_environment_secrets_while_debug_preserves_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let mut inputs = make_inputs(None, None, None);
    inputs
        .environment
        .insert("LANGFUSE_PUBLIC_KEY".into(), "pk-visible-secret".into());
    inputs
        .environment
        .insert("LANGFUSE_SECRET_KEY".into(), "sk-hidden-secret".into());
    let snapshot = ConfigurationSnapshot::resolve(scope, inputs.clone()).unwrap();
    let explanation = snapshot.explain(ConfigurationField::Observability);
    let explanation_rendered = format!("{explanation:?}");
    assert!(!explanation_rendered.contains("pk-visible-secret"));
    assert!(!explanation_rendered.contains("sk-hidden-secret"));
    let debug = format!("{snapshot:?} {inputs:?} {:?}", snapshot.observability());
    assert!(debug.contains("pk-visible-secret"));
    assert!(debug.contains("sk-hidden-secret"));
}
