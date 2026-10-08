use std::collections::BTreeMap;

use super::*;

#[test]
fn configuration_errors_preserve_original_parser_causes() {
    use std::error::Error;
    let error = parse_document(Some("{\n\"token\": "), SourceIdentity::GlobalFile).unwrap_err();
    assert!(error.to_string().contains("line 2"));
    assert!(error.source().unwrap().to_string().contains("EOF"));
}

#[test]
fn every_domain_has_one_shape_and_environment_collection_follows_it() {
    let fields = [
        ConfigurationField::Settings,
        ConfigurationField::McpServers,
        ConfigurationField::McpCache,
        ConfigurationField::BuiltinMcp,
        ConfigurationField::Provider,
        ConfigurationField::Observability,
        ConfigurationField::Ui,
        ConfigurationField::Resources,
        ConfigurationField::Betas,
    ];
    for field in fields {
        assert_eq!(
            DOMAINS
                .iter()
                .filter(|domain| domain.field == field)
                .count(),
            1,
            "每个配置领域必须有且仅有一份来源声明"
        );
    }
    assert_eq!(DOMAINS.len(), fields.len());
    let keys = environment_keys();
    assert!(keys.contains(&mcp::MCP_CACHE_ENV));
    assert!(keys.contains(&mcp::MCP_BUILTIN_ENV));
    for key in provider::ENVIRONMENT_KEYS
        .iter()
        .chain(observability::ENVIRONMENT_KEYS)
    {
        assert!(keys.contains(key), "缺少具名环境来源 {key}");
    }
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn provenance_uses_declared_sources_for_each_domain() {
    let inputs = ConfigurationInputs {
        global: Some("{}".into()),
        workspace: Some("{}".into()),
        project: Some("{}".into()),
        environment: BTreeMap::from([
            (mcp::MCP_CACHE_ENV.into(), "off".into()),
            (mcp::MCP_BUILTIN_ENV.into(), "off".into()),
        ]),
    };
    let directory = tempfile::tempdir().unwrap();
    let scope = ConfigurationScope::new(
        directory.path().join("project"),
        directory.path().join("home/settings.json"),
    )
    .unwrap();
    let snapshot = crate::ConfigurationSnapshot::resolve(scope, inputs).unwrap();
    assert_eq!(
        snapshot.explain(ConfigurationField::McpCache).contributors,
        vec![
            SourceIdentity::Defaults,
            SourceIdentity::GlobalFile,
            SourceIdentity::ProjectMcpFile,
            SourceIdentity::Environment(mcp::MCP_CACHE_ENV.into()),
        ]
    );
    assert_eq!(
        snapshot
            .explain(ConfigurationField::BuiltinMcp)
            .contributors,
        vec![
            SourceIdentity::Defaults,
            SourceIdentity::Environment(mcp::MCP_BUILTIN_ENV.into()),
        ]
    );
    assert_eq!(
        snapshot.explain(ConfigurationField::Settings).contributors,
        vec![
            SourceIdentity::Defaults,
            SourceIdentity::GlobalFile,
            SourceIdentity::WorkspaceFile,
        ]
    );
    assert_eq!(
        snapshot.explain(ConfigurationField::Betas).contributors,
        vec![
            SourceIdentity::Defaults,
            SourceIdentity::GlobalFile,
            SourceIdentity::WorkspaceFile,
        ],
        "flag 覆盖的参与来源是 global + workspace"
    );
    assert!(!snapshot.builtin_mcp_enabled());
}
