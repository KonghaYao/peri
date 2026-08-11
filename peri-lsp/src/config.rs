use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};

// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::lsp`）。
// `LspConfigSource` / `LspServerConfig` 自本文件迁出；本模块保留
// re-export 保兼容（消费方经 `peri_lsp::config` 或 Resources 门面引用）。
pub use peri_acp_types::lsp::{LspConfigSource, LspServerConfig};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LspConfigFile {
    #[serde(default, rename = "lspServers")]
    pub lsp_servers: HashMap<String, LspServerConfig>,
}

/// 展开配置中的环境变量占位符 ${VAR}
pub fn expand_env_vars(config: &mut LspServerConfig) {
    if let Some(ref mut env_map) = config.env {
        let keys: Vec<String> = env_map.keys().cloned().collect();
        for key in keys {
            if let Some(value) = env_map.get(&key) {
                let expanded = expand_var_string(value);
                env_map.insert(key, expanded);
            }
        }
    }
    config.command = expand_var_string(&config.command);
    config.args = config.args.iter().map(|s| expand_var_string(s)).collect();
}

fn expand_var_string(s: &str) -> String {
    expand_var_string_with(s, &HashMap::new())
}

/// 展开 s 中所有 ${VAR} 占位符：优先从 `extra` 映射取值（如注入的
/// CLAUDE_PLUGIN_ROOT），其次进程环境；均未定义则原样保留。
fn expand_var_string_with(s: &str, extra: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            while let Some(&vc) = chars.peek() {
                if vc == '}' {
                    chars.next(); // consume '}'
                    break;
                }
                var_name.push(vc);
                chars.next();
            }
            if !var_name.is_empty() {
                let value = extra
                    .get(&var_name)
                    .cloned()
                    .or_else(|| std::env::var(&var_name).ok());
                if let Some(val) = value {
                    result.push_str(&val);
                } else {
                    result.push_str(&format!("${{{var_name}}}"));
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// 加载全局 LSP 配置（从 settings.json 的 config.lspServers）
pub fn load_global_lsp_config(settings_path: &Path) -> LspConfigFile {
    let mut config = LspConfigFile::default();

    if !settings_path.exists() {
        return config;
    }

    let Ok(content) = std::fs::read_to_string(settings_path) else {
        return config;
    };

    let Ok(per_config) = serde_json::from_str::<serde_json::Value>(&content) else {
        return config;
    };

    let Some(lsp_servers) = per_config.get("config").and_then(|c| c.get("lspServers")) else {
        return config;
    };

    if let Ok(servers) =
        serde_json::from_value::<HashMap<String, LspServerConfig>>(lsp_servers.clone())
    {
        for (name, mut server_config) in servers {
            // name 以 settings.json 的 key 为准（与 `lsp_config_from_plugin`
            // 的 name 语义一致——name 即装配/池侧服务器标识，JSON 内显式
            // name 字段与 key 不一致时以 key 为准，对齐 MCP key 即服务器名）。
            server_config.name = name.clone();
            server_config.source = Some(LspConfigSource::Global(settings_path.to_path_buf()));
            expand_env_vars(&mut server_config);
            config.lsp_servers.insert(name, server_config);
        }
    }

    config
}

/// 从插件 LSP server 配置列表创建 LspServerConfig。
///
/// 注入 `CLAUDE_PLUGIN_ROOT`（插件安装根）到子进程环境，并按注入 env 展开
/// command/args 中的 `${CLAUDE_PLUGIN_ROOT}` 占位符（进程环境未必有该变量）。
pub fn lsp_config_from_plugin(
    plugin_name: &str,
    server_name: &str,
    command: &str,
    args: &[String],
    plugin_install_path: &Path,
    extension_to_language: HashMap<String, String>,
) -> LspServerConfig {
    let full_name = format!("plugin:{}:{}", plugin_name, server_name);
    let mut env = HashMap::new();
    env.insert(
        "CLAUDE_PLUGIN_ROOT".to_string(),
        plugin_install_path.to_string_lossy().to_string(),
    );
    let mut config = LspServerConfig {
        name: full_name,
        command: command.to_string(),
        args: args.to_vec(),
        env: Some(env),
        extension_to_language,
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: Some(LspConfigSource::Plugin {
            plugin_name: plugin_name.to_string(),
        }),
    };
    expand_env_vars(&mut config);
    // expand_env_vars 只查进程环境，而 CLAUDE_PLUGIN_ROOT 仅存在于注入 env，
    // 这里补充按注入 env 展开 command/args（插件根相对命令依赖此语义）。
    let injected_env = config.env.clone().unwrap_or_default();
    config.command = expand_var_string_with(&config.command, &injected_env);
    config.args = config
        .args
        .iter()
        .map(|a| expand_var_string_with(a, &injected_env))
        .collect();
    config
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
