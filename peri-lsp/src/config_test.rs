//! Tests for config_lsp

use super::*;

#[test]
fn test_config_deserialization() {
    let json = r#"{
        "lspServers": {
            "rust-analyzer": {
                "name": "rust-analyzer",
                "command": "rust-analyzer",
                "args": ["--stdio"],
                "extensionToLanguage": {
                    ".rs": "rust"
                }
            }
        }
    }"#;
    let config: LspConfigFile = serde_json::from_str(json).unwrap();
    assert_eq!(config.lsp_servers.len(), 1);
    let ra = &config.lsp_servers["rust-analyzer"];
    assert_eq!(ra.command, "rust-analyzer");
    assert_eq!(ra.args, vec!["--stdio"]);
    assert_eq!(ra.extension_to_language.get(".rs").unwrap(), "rust");
}

#[test]
fn test_config_with_all_fields() {
    let json = r#"{
        "lspServers": {
            "typescript": {
                "name": "typescript-language-server",
                "command": "typescript-language-server",
                "args": ["--stdio"],
                "env": {"NODE_ENV": "production"},
                "extensionToLanguage": {
                    ".ts": "typescript",
                    ".tsx": "typescriptreact"
                },
                "initializationOptions": {"maxTsServerMemory": 8192},
                "disabled": false,
                "maxRestarts": 5,
                "startupTimeout": 30000
            }
        }
    }"#;
    let config: LspConfigFile = serde_json::from_str(json).unwrap();
    let ts = &config.lsp_servers["typescript"];
    assert_eq!(ts.max_restarts, Some(5));
    assert_eq!(ts.startup_timeout, Some(30000));
    assert_eq!(ts.disabled, Some(false));
    assert!(ts.initialization_options.is_some());
}

#[test]
fn test_expand_env_vars() {
    // 设置环境变量用于测试展开
    std::env::set_var("TEST_LSP_VAR", "expanded_value");
    let mut config = LspServerConfig {
        name: "test".to_string(),
        command: "${TEST_LSP_VAR}/bin/server".to_string(),
        args: vec!["--flag".to_string(), "${TEST_LSP_VAR}".to_string()],
        env: Some(HashMap::from([(
            "CUSTOM".to_string(),
            "${TEST_LSP_VAR}".to_string(),
        )])),
        extension_to_language: HashMap::new(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    };
    expand_env_vars(&mut config);
    assert_eq!(config.command, "expanded_value/bin/server");
    assert_eq!(config.args[1], "expanded_value");
    assert_eq!(
        config.env.as_ref().unwrap().get("CUSTOM").unwrap(),
        "expanded_value"
    );
}

#[test]
fn test_expand_env_vars_missing() {
    let mut config = LspServerConfig {
        name: "test".to_string(),
        command: "${NONEXISTENT_VAR}/server".to_string(),
        args: vec![],
        env: None,
        extension_to_language: HashMap::new(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    };
    expand_env_vars(&mut config);
    // 不存在的环境变量原样保留
    assert_eq!(config.command, "${NONEXISTENT_VAR}/server");
}

#[test]
fn test_config_default_values() {
    let json = r#"{"lspServers": {"test": {"command": "test-server"}}}"#;
    let config: LspConfigFile = serde_json::from_str(json).unwrap();
    let test = &config.lsp_servers["test"];
    assert!(test.args.is_empty());
    assert!(test.env.is_none());
    assert!(test.extension_to_language.is_empty());
    assert!(test.disabled.is_none());
    assert!(test.max_restarts.is_none());
}
