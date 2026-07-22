use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use parking_lot::RwLock;

use crate::{
    client::{LspClient, ServerState},
    config::{LspConfigFile, LspConfigSource},
    diagnostics::DiagnosticsRegistry,
    error::LspError,
};

/// LSP 服务器池：管理多个 LSP 服务器实例，按文件扩展名路由
pub struct LspServerPool {
    servers: RwLock<HashMap<String, Arc<LspClient>>>,
    /// 扩展名 -> 服务器名 映射
    extension_map: RwLock<HashMap<String, String>>,
    /// 工作目录
    root_uri: String,
    /// 诊断注册表
    diagnostics: Arc<DiagnosticsRegistry>,
    /// 已初始化的服务器名集合
    initialized: RwLock<HashSet<String>>,
}

#[derive(Debug)]
pub struct LspServerInfo {
    pub name: String,
    pub state: ServerState,
    pub source: Option<LspConfigSource>,
}

impl LspServerPool {
    /// 创建池（惰性初始化，此时不启动任何服务器）
    pub fn new(cwd: &str, config: LspConfigFile) -> Self {
        let diagnostics = Arc::new(DiagnosticsRegistry::new());

        let mut extension_map = HashMap::new();
        let mut servers = HashMap::new();

        for (name, server_config) in &config.lsp_servers {
            if server_config.disabled == Some(true) {
                continue;
            }

            let client = Arc::new(LspClient::new(
                server_config.name.clone(),
                server_config.command.clone(),
                server_config.args.clone(),
                server_config.env.clone().unwrap_or_default(),
                server_config.initialization_options.clone(),
                server_config.max_restarts.unwrap_or(3),
                Arc::clone(&diagnostics),
            ));

            // 注册扩展名路由
            for ext in server_config.extension_to_language.keys() {
                let ext_key = if ext.starts_with('.') {
                    ext.to_lowercase()
                } else {
                    format!(".{}", ext).to_lowercase()
                };
                extension_map.insert(ext_key, name.clone());
            }

            servers.insert(name.clone(), client);
        }

        Self {
            servers: RwLock::new(servers),
            extension_map: RwLock::new(extension_map),
            root_uri: format!("file://{}", cwd),
            diagnostics,
            initialized: RwLock::new(HashSet::new()),
        }
    }

    /// 按需初始化：启动所有未初始化的服务器（用于 workspaceSymbol 等全局操作）
    pub async fn ensure_initialized(&self) -> Result<(), LspError> {
        let to_start: Vec<(String, Arc<LspClient>)> = {
            let initialized = self.initialized.read();
            let guard = self.servers.read();
            guard
                .iter()
                .filter(|(n, _)| !initialized.contains(*n))
                .map(|(n, c)| (n.clone(), Arc::clone(c)))
                .collect()
        };

        if to_start.is_empty() {
            return Ok(());
        }

        let mut failed = Vec::new();
        let total_count = to_start.len();

        for (name, client) in &to_start {
            match client.start(&self.root_uri).await {
                Ok(()) => {
                    tracing::info!(target: "lsp", server = %name, "LSP 服务器启动成功");
                    self.initialized.write().insert(name.clone());
                }
                Err(e) => {
                    tracing::warn!(target: "lsp", server = %name, error = %e, "LSP 服务器启动失败");
                    failed.push(name.clone());
                }
            }
        }

        if failed.len() == total_count {
            return Err(LspError::InitFailed {
                server: "all".to_string(),
                reason: format!("所有 LSP 服务器启动失败: {}", failed.join(", ")),
            });
        }

        Ok(())
    }

    /// 按文件扩展名单独初始化：只启动处理该扩展名的服务器
    /// 如果没有匹配的服务器，返回 NoServerForFile
    pub async fn ensure_server_for_file(&self, file_path: &str) -> Result<(), LspError> {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_default();

        let server_name = {
            let extension_map = self.extension_map.read();
            match extension_map.get(&ext) {
                Some(name) => name.clone(),
                None => {
                    return Err(LspError::NoServerForFile {
                        file_path: file_path.to_string(),
                    });
                }
            }
        };

        // 检查是否已初始化
        {
            let initialized = self.initialized.read();
            if initialized.contains(&server_name) {
                return Ok(());
            }
        }

        // 只启动匹配的服务器
        let client = {
            let servers = self.servers.read();
            match servers.get(&server_name) {
                Some(c) => Arc::clone(c),
                None => {
                    return Err(LspError::NoServerForFile {
                        file_path: file_path.to_string(),
                    });
                }
            }
        };

        match client.start(&self.root_uri).await {
            Ok(()) => {
                tracing::info!(target: "lsp", server = %server_name, "LSP 服务器启动成功");
                self.initialized.write().insert(server_name);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(target: "lsp", server = %server_name, error = %e, "LSP 服务器启动失败");
                Err(LspError::InitFailed {
                    server: server_name,
                    reason: e.to_string(),
                })
            }
        }
    }

    /// 根据文件路径查找对应的 LSP 服务器（按扩展名路由）
    pub fn server_for_file(&self, file_path: &str) -> Option<Arc<LspClient>> {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_default();

        let extension_map = self.extension_map.read();
        let server_name = extension_map.get(&ext)?;
        let servers = self.servers.read();
        servers.get(server_name).cloned()
    }

    /// 获取所有已就绪的服务器列表
    pub fn server_info(&self) -> Vec<LspServerInfo> {
        let servers = self.servers.read();
        servers
            .values()
            .map(|c| LspServerInfo {
                name: c.name().to_string(),
                state: c.state(),
                source: None,
            })
            .collect()
    }

    /// 获取诊断注册表
    pub fn diagnostics(&self) -> Arc<DiagnosticsRegistry> {
        Arc::clone(&self.diagnostics)
    }

    /// 检查是否有任何可用的 LSP 服务器
    pub fn has_servers(&self) -> bool {
        !self.servers.read().is_empty()
    }

    /// 优雅关闭所有服务器
    pub async fn shutdown(&self) {
        let servers: Vec<(String, Arc<LspClient>)> = {
            let guard = self.servers.read();
            guard
                .iter()
                .map(|(n, c)| (n.clone(), Arc::clone(c)))
                .collect()
        };
        for (name, client) in servers.iter() {
            tracing::info!(target: "lsp", server = %name, "正在关闭 LSP 服务器");
            client.shutdown().await;
        }
        self.initialized.write().clear();
    }

    /// 动态添加一个 LSP 服务器（如果池已初始化，自动启动新服务器）
    pub async fn add_server(&self, config: crate::config::LspServerConfig) {
        if config.disabled == Some(true) {
            return;
        }

        let name = config.name.clone();
        let client = Arc::new(LspClient::new(
            config.name,
            config.command,
            config.args,
            config.env.unwrap_or_default(),
            config.initialization_options,
            config.max_restarts.unwrap_or(3),
            Arc::clone(&self.diagnostics),
        ));

        for ext in config.extension_to_language.keys() {
            let ext_key = if ext.starts_with('.') {
                ext.to_lowercase()
            } else {
                format!(".{}", ext).to_lowercase()
            };
            self.extension_map.write().insert(ext_key, name.clone());
        }

        self.servers.write().insert(name.clone(), client.clone());

        // 如果池已有已初始化的服务器，立即启动新服务器
        if !self.initialized.read().is_empty() {
            match client.start(&self.root_uri).await {
                Ok(()) => {
                    tracing::info!(target: "lsp", server = %name, "动态添加的 LSP 服务器启动成功");
                    self.initialized.write().insert(name);
                }
                Err(e) => {
                    tracing::warn!(target: "lsp", server = %name, error = %e, "动态添加的 LSP 服务器启动失败")
                }
            }
        }
    }

    /// 获取任意一个已就绪的服务器（用于 workspaceSymbol 等全局操作）
    pub fn any_server(&self) -> Option<Arc<LspClient>> {
        let servers = self.servers.read();
        servers.values().find(|c| c.is_ready()).cloned()
    }

    /// 获取工作目录 URI
    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }

    /// 获取所有服务器实例（用于重连等操作）
    pub fn all_servers(&self) -> Vec<Arc<LspClient>> {
        self.servers.read().values().cloned().collect()
    }
}

#[cfg(test)]
#[path = "pool_test.rs"]
mod tests;
