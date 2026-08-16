//! Plugin / marketplace 命令 handler：install / uninstall / toggle / search /
//! update / refresh 与插件事件推送辅助（自 requests.rs 拆出，请求分发见
//! `host/requests.rs`）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use peri_acp_types::event_data::{
    PluginActionResult, PluginSearchResult, PluginSnapshot, PluginSnapshotEntry,
};
use peri_acp_types::PeriCaps;
use serde_json::Value;

use super::super::{AcpServerConfig, SessionState};
use crate::transport::types::AcpError;

/// Phase 6 B3：插件 install / uninstall 成功后刷新 plugin 域命令条目——
/// 注销全部旧条目 → 重载已启用插件 → 重新注册（`reconcile` 单次写锁原子
/// 完成，任一内容变化只触发**一次** `on_change` → 投影推送，不经 TUI
/// 协议）。
///
/// 重载失败 → 注销全部旧条目（plugin 域保持空：磁盘状态已变，过时条目
/// 不得残留展示）+ 日志告警，不阻塞 RPC 回包。
///
/// Phase 6 遗留登记（P2-4，跨主题确认事项）：插件 mcpServers 变更
/// （install/uninstall 改插件 manifest 的 mcpServers）**无 client 池刷新
/// 触发点**——`McpPoolPort` 仅暴露 shutdown/snapshot，池配置为装配时
/// 快照（`run_initialize` 一次性读取聚合配置含插件 mcpServers，
/// assemble.rs / stdio/init.rs），`reconnect(name)` 仅按既有配置键重连，
/// 无法接入新装插件的服务器；新装插件 `mcp:*` 命令条目依赖既有池重连
/// 机制 + A3 发现链路自愈，需下次装配/会话重启生效，未在本 Phase 触发。
fn refresh_plugin_command_entries(
    cfg: &AcpServerConfig,
    session_id: &str,
    claude_dir: &Path,
    session_cwd: Option<&str>,
) {
    let Some(command_registry) = cfg.session_manager.command_registry_for(session_id) else {
        tracing::warn!(
            session_id,
            "plugin 命令刷新：无 session 级命令注册表，跳过（RPC 回包不受影响）"
        );
        return;
    };
    // stale = 当前 plugin 域全部条目（reconcile 精确键注销，未命中静默跳过）。
    let stale: Vec<String> = command_registry
        .snapshot()
        .iter()
        .filter(|e| e.fullname.to_lowercase().starts_with("plugin:"))
        .map(|e| e.fullname.clone())
        .collect();
    // 重载：与装配面同源（`load_enabled_plugins` → all_commands 聚合）；
    // 无 session 上下文（session_cwd = None）时仅用户级 enabledPlugins。
    let fresh_commands = match peri_middlewares::plugin::load_enabled_plugins(
        claude_dir,
        session_cwd.map(Path::new),
    ) {
        Ok(plugins) => plugins
            .iter()
            .flat_map(|p| p.commands.clone())
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "插件重载失败：plugin 域清空（保留空 plugin 域），不阻塞 RPC 回包"
            );
            Vec::new()
        }
    };
    let (removed, added) = command_registry.reconcile(
        &stale,
        peri_middlewares::plugin::plugin_route_entries(&fresh_commands),
    );
    tracing::info!(
        session_id,
        removed,
        added,
        "插件命令条目动态刷新完成（install/uninstall 后；注册表 on_change 已触发投影推送）"
    );
}

pub(super) async fn handle_install(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'name'"))?;
    let marketplace = params
        .get("marketplace")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'marketplace'"))?;
    let scope_str = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("user");
    let scope = match scope_str {
        "project" => peri_acp_types::plugin::InstallScope::Project,
        "local" => peri_acp_types::plugin::InstallScope::Local,
        _ => peri_acp_types::plugin::InstallScope::User,
    };
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let claude_dir = peri_middlewares::plugin::claude_home();
    let cache_dir = cfg.plugin_manager.cache_dir();

    let caps = cfg.session_manager.get_caps(session_id);

    match cfg
        .plugin_manager
        .install(name, marketplace, scope, &cache_dir, &claude_dir)
        .await
    {
        Ok(installed) => {
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                "install",
                name,
                true,
                None,
                &caps,
            )
            .await;
            let _ = push_plugin_snapshot(
                transport.as_ref(),
                session_id,
                &cfg.plugin_manager.snapshot(&claude_dir),
                &caps,
            )
            .await;
            // Phase 6 B3：install 成功 → plugin 域命令条目动态刷新
            //（注册表 on_change 自动触发投影推送；重载失败 → 保留
            // 空 plugin 域 + 告警，不阻塞回包）
            // 遗留登记（P2-4）：插件 mcpServers 变更自愈依赖既有池
            // 重连机制（池为装配时快照），未在本 Phase 触发，详见
            // `refresh_plugin_command_entries` doc 注释。
            refresh_plugin_command_entries(
                cfg,
                session_id,
                &claude_dir,
                sessions.get(session_id).map(|s| s.cwd.as_str()),
            );
            Ok(serde_json::json!({ "success": true, "plugin": installed.id }))
        }
        Err(e) => {
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                "install",
                name,
                false,
                Some(&e.to_string()),
                &caps,
            )
            .await;
            Err(AcpError::new(-32603, e.to_string()))
        }
    }
}

pub(super) async fn handle_uninstall(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let plugin_id = params
        .get("pluginId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'pluginId'"))?;
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let claude_dir = peri_middlewares::plugin::claude_home();

    let caps = cfg.session_manager.get_caps(session_id);

    match cfg.plugin_manager.uninstall(plugin_id, &claude_dir).await {
        Ok(()) => {
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                "uninstall",
                plugin_id,
                true,
                None,
                &caps,
            )
            .await;
            let _ = push_plugin_snapshot(
                transport.as_ref(),
                session_id,
                &cfg.plugin_manager.snapshot(&claude_dir),
                &caps,
            )
            .await;
            // Phase 6 B3：uninstall 成功 → plugin 域命令条目动态刷新
            //（注册表 on_change 自动触发投影推送；重载失败 → 保留
            // 空 plugin 域 + 告警，不阻塞回包）
            // 遗留登记（P2-4）：插件 mcpServers 变更自愈依赖既有池
            // 重连机制（池为装配时快照），未在本 Phase 触发，详见
            // `refresh_plugin_command_entries` doc 注释。
            refresh_plugin_command_entries(
                cfg,
                session_id,
                &claude_dir,
                sessions.get(session_id).map(|s| s.cwd.as_str()),
            );
            Ok(serde_json::json!({ "success": true }))
        }
        Err(e) => {
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                "uninstall",
                plugin_id,
                false,
                Some(&e.to_string()),
                &caps,
            )
            .await;
            Err(AcpError::new(-32603, e.to_string()))
        }
    }
}

pub(super) async fn handle_toggle(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let plugin_id = params
        .get("pluginId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'pluginId'"))?;
    let enable = params
        .get("enable")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let scope_str = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("user");
    let scope = match scope_str {
        "project" => peri_acp_types::plugin::InstallScope::Project,
        "local" => peri_acp_types::plugin::InstallScope::Local,
        _ => peri_acp_types::plugin::InstallScope::User,
    };
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let claude_dir = peri_middlewares::plugin::claude_home();

    let result = cfg
        .plugin_manager
        .set_enabled(plugin_id, scope, &claude_dir, enable);

    let caps = cfg.session_manager.get_caps(session_id);

    match result {
        Ok(()) => {
            let action = if enable { "enable" } else { "disable" };
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                action,
                plugin_id,
                true,
                None,
                &caps,
            )
            .await;
            let _ = push_plugin_snapshot(
                transport.as_ref(),
                session_id,
                &cfg.plugin_manager.snapshot(&claude_dir),
                &caps,
            )
            .await;
            Ok(serde_json::json!({ "success": true }))
        }
        Err(e) => {
            let action = if enable { "enable" } else { "disable" };
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                action,
                plugin_id,
                false,
                Some(&e.to_string()),
                &caps,
            )
            .await;
            Err(AcpError::new(-32603, e.to_string()))
        }
    }
}

pub(super) async fn handle_search(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let query = params
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'query'"))?;
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let cache_dir = cfg.plugin_manager.cache_dir();
    let results = search_marketplace_plugins(query, &cache_dir);

    let caps = cfg.session_manager.get_caps(session_id);
    let _ = push_plugin_search_result(transport.as_ref(), session_id, query, &results, &caps).await;
    Ok(serde_json::json!({ "results": results.iter().map(|r| {
        serde_json::json!({
            "name": r.name,
            "version": r.version,
            "description": r.description,
            "marketplace": r.marketplace,
        })
    }).collect::<Vec<_>>() }))
}

pub(super) async fn handle_update(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let plugin_id = params
        .get("pluginId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'pluginId'"))?;
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let claude_dir = peri_middlewares::plugin::claude_home();
    let cache_dir = cfg.plugin_manager.cache_dir();

    let caps = cfg.session_manager.get_caps(session_id);

    match cfg
        .plugin_manager
        .update(plugin_id, &cache_dir, &claude_dir)
        .await
    {
        Ok(updated) => {
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                "update",
                plugin_id,
                true,
                None,
                &caps,
            )
            .await;
            let _ = push_plugin_snapshot(
                transport.as_ref(),
                session_id,
                &cfg.plugin_manager.snapshot(&claude_dir),
                &caps,
            )
            .await;
            Ok(serde_json::json!({ "success": true, "plugin": updated.id }))
        }
        Err(e) => {
            let _ = push_plugin_action_result(
                transport.as_ref(),
                session_id,
                "update",
                plugin_id,
                false,
                Some(&e.to_string()),
                &caps,
            )
            .await;
            Err(AcpError::new(-32603, e.to_string()))
        }
    }
}

pub(super) async fn handle_refresh(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'name'"))?;
    // 定位 known_marketplaces 条目 + 刷新（实现留在插件管理端口，
    // 命令面不触碰 marketplace 目录结构）
    match cfg.plugin_manager.refresh_marketplace(name).await {
        Ok(plugin_count) => Ok(serde_json::json!({ "success": true, "pluginCount": plugin_count })),
        Err(e) => Err(AcpError::new(-32603, e)),
    }
}

// ── Plugin event pushers ──────────────────────────────────────────────────

async fn push_plugin_action_result(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    action: &str,
    plugin_name: &str,
    success: bool,
    error: Option<&str>,
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let payload = PluginActionResult {
        action: action.to_string(),
        plugin_name: plugin_name.to_string(),
        success,
        error: error.map(|s| s.to_string()),
    };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-action-result",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable_event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-action-result");
    }
}

async fn push_plugin_snapshot(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    plugins: &[PluginSnapshotEntry],
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let payload = PluginSnapshot {
        plugins: plugins.to_vec(),
    };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-snapshot",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable_event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-snapshot");
    }
}

async fn push_plugin_search_result(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    query: &str,
    results: &[PluginSnapshotEntry],
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let payload = PluginSearchResult {
        query: query.to_string(),
        results: results.to_vec(),
        from_cache: true,
    };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-search-result",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable_event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-search-result");
    }
}

fn search_marketplace_plugins(
    query: &str,
    cache_dir: &std::path::Path,
) -> Vec<PluginSnapshotEntry> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let mp_dir = entry.path();
            let mp_name = mp_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let manifest_path = mp_dir.join("marketplace.json");
            let Ok(content) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            if let Some(plugins) = manifest.get("plugins").and_then(|v| v.as_array()) {
                for p in plugins {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let desc = p.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    if name.to_lowercase().contains(&query_lower)
                        || desc.to_lowercase().contains(&query_lower)
                    {
                        results.push(PluginSnapshotEntry {
                            name: name.to_string(),
                            version: p
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            enabled: false,
                            root: String::new(),
                            description: desc.to_string(),
                            marketplace: mp_name.clone(),
                            author: p
                                .get("author")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            skills_count: 0,
                            commands_count: 0,
                            agents_count: 0,
                            mcp_count: 0,
                            install_scope: String::new(),
                            load_error: None,
                        });
                    }
                }
            }
        }
    }
    results
}
