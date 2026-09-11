//! 连接状态写入、面板快照与初始化/运行中通知投影。

use super::{ClientStatus, McpClientHandle, McpClientPool, OAuthStatus, ServerInfo};
use std::sync::Arc;

/// 供状态与日志使用的 MCP 错误文本清洗：移除 URL query，遮蔽常见凭据键值。
/// 不应将原始底层错误链直接投影到 UI 或日志。
pub fn redact_mcp_error(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for token in input.split_whitespace() {
        let token = if let Some((prefix, _)) = token.split_once('?') {
            if prefix.starts_with("http://") || prefix.starts_with("https://") {
                format!("{prefix}?…")
            } else {
                token.to_string()
            }
        } else {
            token.to_string()
        };
        let lower = token.to_ascii_lowercase();
        if ["token=", "password=", "secret=", "api_key=", "apikey="]
            .iter()
            .any(|key| lower.contains(key))
        {
            output.push_str("[redacted]");
        } else {
            output.push_str(&token);
        }
        output.push(' ');
    }
    output.trim_end().to_string()
}

pub(super) fn mcp_status_label(status: &ClientStatus) -> &'static str {
    match status {
        ClientStatus::Connected => "connected",
        ClientStatus::Failed(_) => "failed",
        ClientStatus::Disconnected => "disconnected",
        ClientStatus::Disabled => "disabled",
        ClientStatus::Uninitialized => "uninitialized",
    }
}

pub(super) fn mcp_error_summary(status: &ClientStatus) -> Option<String> {
    let ClientStatus::Failed(reason) = status else {
        return None;
    };
    let summary = redact_mcp_error(reason.lines().next().unwrap_or_default().trim());
    let summary: String = summary.chars().take(160).collect();
    (!summary.is_empty()).then_some(summary)
}

/// MCP 状态变化 → 通知文本（每台一行：上线带工具数，失败报名字 + 错误）。
pub(crate) fn status_change_text(name: &str, status: &ClientStatus, tool_count: usize) -> String {
    match status {
        ClientStatus::Connected => {
            format!("MCP: {name} connected ({tool_count} tools)")
        }
        ClientStatus::Failed(reason) => {
            format!("MCP: {name} failed: {reason}")
        }
        ClientStatus::Disconnected => {
            format!("MCP: {name} disconnected")
        }
        ClientStatus::Disabled => {
            format!("MCP: {name} disabled")
        }
        ClientStatus::Uninitialized => {
            format!("MCP: {name} uninitialized")
        }
    }
}

impl McpClientPool {
    pub(crate) fn insert_failed(pool: &Arc<Self>, name: &str, reason: String) {
        let old_status = {
            let _admission = pool.lifecycle_registration.lock();
            if !pool.is_open() {
                return;
            }
            let (source, url) = pool
                .configs
                .read()
                .get(name)
                .map(|c| (c.source.clone(), c.url.clone()))
                .unwrap_or((None, None));
            let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
            let handle = Arc::new(McpClientHandle {
                name: name.to_string(),
                version: None,
                cache_version: None,
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason.clone()),
                oauth_status: OAuthStatus::default(),
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            });
            pool.advance_handle_generation(&handle);
            pool.clients.write().insert(name.to_string(), handle);
            old_status
        };
        pool.record_status_change(name, old_status.as_ref());
        peri_agent::metrics::emit(
            "mcp.error",
            serde_json::json!({
                "server": name,
                "tool": "connect",
                "error": reason,
            }),
            None,
            None,
        );
    }

    /// 插入需要 OAuth 授权的服务器（HTTP 传输收到 401/AuthRequired 时使用）
    pub(crate) fn insert_needs_auth(pool: &Arc<Self>, name: &str, reason: String) {
        tracing::info!(server = %name, "HTTP 服务器需要 OAuth 授权，可在 MCP 面板按 r 键触发");
        let old_status = {
            let _admission = pool.lifecycle_registration.lock();
            if !pool.is_open() {
                return;
            }
            let (source, url) = pool
                .configs
                .read()
                .get(name)
                .map(|c| (c.source.clone(), c.url.clone()))
                .unwrap_or((None, None));
            let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
            let handle = Arc::new(McpClientHandle {
                name: name.to_string(),
                version: None,
                cache_version: None,
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason),
                oauth_status: OAuthStatus::NeedsAuthorization,
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            });
            pool.advance_handle_generation(&handle);
            pool.clients.write().insert(name.to_string(), handle);
            old_status
        };
        pool.record_status_change(name, old_status.as_ref());
    }

    /// 检测错误是否为 HTTP 401 认证错误
    pub(crate) fn is_auth_required_error(error: &str, transport_is_http: bool) -> bool {
        transport_is_http && (error.contains("Auth required") || error.contains("AuthRequired"))
    }

    pub fn server_infos(&self) -> Vec<ServerInfo> {
        self.clients
            .read()
            .values()
            .map(|h| ServerInfo {
                name: h.name.clone(),
                version: h.version.clone(),
                cache_version: h.cache_version.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                status_label: mcp_status_label(&h.status).to_string(),
                error_summary: mcp_error_summary(&h.status),
                cache_status: self.cache_status_for(&h.name),
                tool_count: h.tools.len(),
                resource_count: h.resources.len(),
                oauth_status: h.oauth_status.clone(),
                source: h.source.clone(),
                url: h.url.clone(),
                plugin_source: self.plugin_source_of(&h.name),
            })
            .collect()
    }

    /// 返回所有 MCP 服务器信息（合并 configs + clients）
    ///
    /// config 中有但 clients 中没有的 server 会被标记为 Uninitialized。
    /// 这覆盖了连接失败后被移除、运行时新增配置、以及 disabled 后被清理等场景。
    pub fn all_server_infos(&self) -> Vec<ServerInfo> {
        let clients = self.clients.read();
        let configs = self.configs.read();

        let mut result: Vec<ServerInfo> = Vec::new();

        // 先遍历 clients 表中的所有条目
        for h in clients.values() {
            result.push(ServerInfo {
                name: h.name.clone(),
                version: h.version.clone(),
                cache_version: h.cache_version.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                status_label: mcp_status_label(&h.status).to_string(),
                error_summary: mcp_error_summary(&h.status),
                cache_status: self.cache_status_for(&h.name),
                tool_count: h.tools.len(),
                resource_count: h.resources.len(),
                oauth_status: h.oauth_status.clone(),
                source: h.source.clone(),
                url: h.url.clone(),
                plugin_source: self.plugin_source_of(&h.name),
            });
        }

        // 遍历 configs，补充 clients 中不存在的条目（标记为 Uninitialized）
        for (name, sc) in configs.iter() {
            if !clients.contains_key(name) {
                result.push(ServerInfo {
                    version: None,
                    cache_version: None,
                    name: name.clone(),
                    transport_type: if sc.url.is_some() { "http" } else { "stdio" }.to_string(),
                    status: ClientStatus::Uninitialized,
                    status_label: "uninitialized".to_string(),
                    error_summary: None,
                    cache_status: self.cache_status_for(name),
                    tool_count: 0,
                    resource_count: 0,
                    oauth_status: OAuthStatus::default(),
                    source: sc.source.clone(),
                    url: sc.url.clone(),
                    plugin_source: self.plugin_source_of(name),
                });
            }
        }

        result
    }

    // ── 状态变化统一出口（上下线通知） ──────────────────────────────────────

    /// 标记初始化完成。此后发生的状态变化才产生上下线通知（初始化期间的
    /// 连接结果由首 turn 概览覆盖，不逐条通知）。
    pub fn mark_initialized(&self) {
        self.initialized
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 记录一次状态变化（统一出口）。
    ///
    /// `old` 为调用方在修改客户端表**之前**捕获的旧状态；`None` 表示表内
    /// 此前不存在（首次插入/重建，无"变化"语义，不通知）。调用方完成
    /// 状态写入后调用本方法。
    ///
    /// 仅当：初始化已完成 + 表内存在且状态确实变化时，生成一行通知文本
    /// （`status_change_text`）写入 `pending_changes` 缓冲（McpMiddleware
    /// 经 before_model drain 后以 Info 消息推送进模型上下文），并调用
    /// notifier 回调（发布 system-notification 给 TUI 通知面）。
    pub(crate) fn record_status_change(&self, name: &str, old: Option<&ClientStatus>) {
        if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(old) = old else { return };
        let clients = self.clients.read();
        let Some(handle) = clients.get(name) else {
            return;
        };
        if &handle.status == old {
            return;
        }
        let text = status_change_text(name, &handle.status, handle.tools.len());
        self.pending_changes.lock().push(text.clone());
        let notifier = self.notifier.read().clone();
        if let Some(notifier) = notifier {
            notifier(&text);
        }
    }

    /// 注入状态变化通知回调（发布 system-notification 事件；装配时调用）。
    pub fn set_notifier(&self, notifier: Box<dyn Fn(&str) + Send + Sync>) {
        let _admission = self.lifecycle_registration.lock();
        if self.is_open() {
            *self.notifier.write() = Some(Arc::from(notifier));
        }
    }

    /// 初始化收口后补发初始连接通知（仅 notifier 回调，不进
    /// `pending_changes`——初始连接概览由首 turn 的 `first_turn_reminder`
    /// 覆盖，不重复注入模型上下文）。
    ///
    /// 背景：`run_initialize` 直接插入 Connected handle（不经过
    /// [`Self::record_status_change`]），且 `mark_initialized` 在全部连接
    /// 之后才置位——初始化期间的连接事件永远不产生 notifier 调用。
    /// 装配面 / session 预热挂载的连接事件 notifier
    /// （`attach_connection_notifier`）因此收不到初始连接，只有重连 /
    /// OAuth 完成（`mark_initialized` 之后的 `record_status_change`）才能
    /// 触发。本方法在初始化收口时补发一次，使「刚进入、未说话」场景下
    /// 连接完成的 server 也能立即驱动 skill 发现。
    ///
    /// 锁序：先持 clients 读锁收集文本（短临界区），再持 notifier 读锁
    /// 逐条回调。回调可能重入 pool 读锁（`run_ensure_discovery`）——
    /// parking_lot 读锁可重入，与 `record_status_change` 锁内调用先例一致。
    pub fn notify_initial_connections(&self) {
        let texts: Vec<String> = {
            let clients = self.clients.read();
            clients
                .values()
                .filter(|h| matches!(h.status, ClientStatus::Connected))
                .map(|h| status_change_text(&h.name, &h.status, h.tools.len()))
                .collect()
        };
        let notifier = self.notifier.read().clone();
        if let Some(notifier) = notifier {
            for text in texts {
                notifier(&text);
            }
        }
    }

    /// 取出待注入的状态变化文本（McpMiddleware::before_model 调用；恰好一次）。
    pub(crate) fn drain_pending_changes(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_changes.lock())
    }
}
