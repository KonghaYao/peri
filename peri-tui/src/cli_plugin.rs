//! plugin 子命令实现：list / install / uninstall / marketplace add/list/remove

use anyhow::Result;
use chrono::Local;
use std::path::Path;

use crate::cli_args::PluginScope;
use peri_middlewares::plugin::{
    KnownMarketplace, MarketplaceSource, load_known_marketplaces,
    marketplace::{MarketplaceManager, refresh_marketplace},
    parse_marketplace_input, save_known_marketplaces, update_plugin,
};

struct PluginListEntry {
    id: String,
    name: String,
    version: String,
    marketplace: String,
    enabled: bool,
    scope: String,
}

fn load_plugins() -> Vec<PluginListEntry> {
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let installed = peri_middlewares::plugin::config::load_installed_plugins(Some(&plugins_path))
        .unwrap_or_default();

    installed
        .plugins
        .into_iter()
        .map(|p| PluginListEntry {
            id: p.id,
            name: p.name,
            version: p.version,
            marketplace: p.marketplace,
            enabled: true,
            scope: match p.scope {
                peri_middlewares::plugin::InstallScope::User => "user",
                peri_middlewares::plugin::InstallScope::Project => "project",
                peri_middlewares::plugin::InstallScope::Local => "local",
            }
            .to_string(),
        })
        .collect()
}

pub fn run_plugin_list(json: bool) -> Result<()> {
    let entries = load_plugins();

    if json {
        let json_entries: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "name": e.name,
                    "version": e.version,
                    "marketplace": e.marketplace,
                    "enabled": e.enabled,
                    "scope": e.scope,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_entries)?);
    } else if entries.is_empty() {
        println!("未安装任何插件。");
    } else {
        println!("{:<40} {:<10} {:<15} 状态", "ID", "版本", "市场");
        println!("{}", "-".repeat(80));
        for e in &entries {
            let status = if e.enabled { "已启用" } else { "已禁用" };
            println!(
                "{:<40} {:<10} {:<15} {status}",
                e.id, e.version, e.marketplace,
            );
        }
    }
    Ok(())
}

pub async fn run_plugin_install(plugin_name: &str, scope_str: &str) -> Result<()> {
    let scope: PluginScope = scope_str.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");
    let cache_dir = peri_middlewares::plugin::config::marketplaces_cache_dir();

    let (name, marketplace) = if let Some((name, mkt)) = plugin_name.split_once('@') {
        (name, mkt.to_string())
    } else {
        // 遍历所有已知 marketplace 搜索插件
        let found = peri_middlewares::plugin::find_plugin_in_marketplaces(plugin_name, &cache_dir)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        (plugin_name, found)
    };

    let result = peri_middlewares::plugin::install_plugin(
        name,
        &marketplace,
        scope.into(),
        &cache_dir,
        &claude_dir,
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("安装失败: {e}"))?;

    println!(
        "已安装: {} v{} (scope: {})",
        result.id, result.version, scope_str
    );
    Ok(())
}

pub async fn run_plugin_uninstall(plugin_id: &str, _scope_str: Option<&str>) -> Result<()> {
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");

    peri_middlewares::plugin::uninstall_plugin(plugin_id, &claude_dir, None)
        .await
        .map_err(|e| anyhow::anyhow!("卸载失败: {e}"))?;

    println!("已卸载: {}", plugin_id);
    Ok(())
}

pub async fn run_marketplace_add(source: &str) -> Result<()> {
    let marketplace_source = parse_marketplace_input(source)
        .map_err(|e| anyhow::anyhow!("无效的 marketplace source: {e}"))?;

    let name = MarketplaceManager::extract_name(&marketplace_source);

    let mut marketplaces =
        load_known_marketplaces(None).map_err(|e| anyhow::anyhow!("加载 marketplace 失败: {e}"))?;

    // 检查是否已存在：如果已有 valid install_location 则真重复，否则是旧残留（需刷新）
    if let Some(existing) = marketplaces
        .iter()
        .position(|mkt| MarketplaceManager::extract_name(&mkt.source) == name)
    {
        let old = &marketplaces[existing];
        if !old.install_location.is_empty() {
            println!("marketplace \"{}\" 已存在，跳过", name);
            return Ok(());
        }
        // 旧残留（install_location 为空），删除后重新添加
        marketplaces.remove(existing);
    }

    // 立即 clone/fetch marketplace，不等到下次启动
    let (manifest, install_location) = refresh_marketplace(&marketplace_source, &name)
        .await
        .map_err(|e| anyhow::anyhow!("无法拉取 marketplace: {e}"))?;

    // 用 manifest 里的 name 覆盖从 source 提取的名称
    let actual_name = manifest.name;

    let now = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    marketplaces.push(KnownMarketplace {
        source: marketplace_source,
        install_location,
        auto_update: false,
        last_updated: now,
    });

    save_known_marketplaces(&marketplaces, None)
        .map_err(|e| anyhow::anyhow!("保存 marketplace 失败: {e}"))?;

    println!("已添加 marketplace: {}", actual_name);
    Ok(())
}

pub fn run_marketplace_list() -> Result<()> {
    let marketplaces =
        load_known_marketplaces(None).map_err(|e| anyhow::anyhow!("加载 marketplace 失败: {e}"))?;

    if marketplaces.is_empty() {
        println!("没有注册的 marketplace。");
        return Ok(());
    }

    println!("{:<30} {:<60} {:<20}", "名称", "来源", "最后更新");
    println!("{}", "-".repeat(112));
    for mkt in &marketplaces {
        let name = MarketplaceManager::extract_name(&mkt.source);
        let source_str = match &mkt.source {
            MarketplaceSource::GitHub { repo } => format!("github:{}", repo),
            MarketplaceSource::Git { url } => format!("git:{}", url),
            MarketplaceSource::Url { url } => url.clone(),
            MarketplaceSource::File { path } => format!("file:{}", path),
            MarketplaceSource::Directory { path } => format!("dir:{}", path),
            MarketplaceSource::Npm { package } => format!("npm:{}", package),
        };
        println!("{:<30} {:<60} {:<20}", name, source_str, mkt.last_updated);
    }
    Ok(())
}

pub fn run_marketplace_remove(name: &str) -> Result<()> {
    let marketplaces =
        load_known_marketplaces(None).map_err(|e| anyhow::anyhow!("加载 marketplace 失败: {e}"))?;

    let original_len = marketplaces.len();

    // D-del: 找到被删除的 entry，用于清理磁盘缓存
    let removed_location = marketplaces
        .iter()
        .find(|mkt| MarketplaceManager::extract_name(&mkt.source) == name)
        .map(|km| km.install_location.clone());

    let filtered: Vec<KnownMarketplace> = marketplaces
        .into_iter()
        .filter(|mkt| MarketplaceManager::extract_name(&mkt.source) != name)
        .collect();

    if filtered.len() == original_len {
        anyhow::bail!("未找到名为 \"{}\" 的 marketplace", name);
    }

    save_known_marketplaces(&filtered, None)
        .map_err(|e| anyhow::anyhow!("保存 marketplace 失败: {e}"))?;

    // D-del: 清除磁盘缓存目录
    if let Some(ref loc) = removed_location {
        let install_path = std::path::Path::new(loc);
        if !loc.is_empty() && install_path.exists() {
            std::fs::remove_dir_all(install_path)?;
            println!("已清除缓存目录: {}", install_path.display());
        }
    }

    println!("已删除 marketplace: {}", name);
    Ok(())
}

// ── marketplace update ──────────────────────────────────────────────────

pub async fn run_marketplace_update(name: &str) -> Result<()> {
    let marketplaces =
        load_known_marketplaces(None).map_err(|e| anyhow::anyhow!("加载 marketplace 失败: {e}"))?;

    let entry_index = marketplaces
        .iter()
        .position(|mkt| MarketplaceManager::extract_name(&mkt.source) == name);
    let entry_index = match entry_index {
        Some(i) => i,
        None => anyhow::bail!("未找到名为 \"{}\" 的 marketplace", name),
    };

    let entry = &marketplaces[entry_index];
    let (manifest, install_location) = refresh_marketplace(&entry.source, name)
        .await
        .map_err(|e| anyhow::anyhow!("无法刷新 marketplace: {e}"))?;

    let mut updated = marketplaces;
    let now = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    updated[entry_index].install_location = install_location;
    updated[entry_index].last_updated = now;

    save_known_marketplaces(&updated, None)
        .map_err(|e| anyhow::anyhow!("保存 marketplace 失败: {e}"))?;

    let actual_name = manifest.name;
    println!("已更新 marketplace: {}", actual_name);
    Ok(())
}

// ── plugin enable ───────────────────────────────────────────────────────

pub fn run_plugin_enable(plugin_id: &str, scope_str: &str) -> Result<()> {
    let scope: PluginScope = scope_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("无效的 scope: {e}"))?;
    let install_scope: peri_middlewares::plugin::InstallScope = scope.into();
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");

    peri_middlewares::plugin::update_enabled_plugins(plugin_id, install_scope, &claude_dir, None)
        .map_err(|e| anyhow::anyhow!("启用插件失败: {e}"))?;

    println!("已启用插件: {} (scope: {})", plugin_id, scope_str);
    Ok(())
}

// ── plugin disable ──────────────────────────────────────────────────────

pub fn run_plugin_disable(plugin_id: &str, scope_str: &str) -> Result<()> {
    let scope: PluginScope = scope_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("无效的 scope: {e}"))?;
    let install_scope: peri_middlewares::plugin::InstallScope = scope.into();
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");

    peri_middlewares::plugin::remove_from_enabled_plugins(
        plugin_id,
        &install_scope,
        &claude_dir,
        None,
    )
    .map_err(|e| anyhow::anyhow!("禁用插件失败: {e}"))?;

    println!("已禁用插件: {} (scope: {})", plugin_id, scope_str);
    Ok(())
}

// ── plugin update ───────────────────────────────────────────────────────

pub async fn run_plugin_update(plugin_id: &str, scope_str: &str) -> Result<()> {
    let scope: PluginScope = scope_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("无效的 scope: {e}"))?;
    let _install_scope: peri_middlewares::plugin::InstallScope = scope.into();
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");
    let cache_dir = peri_middlewares::plugin::config::marketplaces_cache_dir();

    match update_plugin(plugin_id, &cache_dir, &claude_dir, None).await {
        Ok(installed) => {
            println!("已更新插件: {} v{}", installed.id, installed.version);
        }
        Err(e) => {
            // Check if already up-to-date
            let err_str = e.to_string();
            if err_str.contains("already up to date") || err_str.contains("Already up to date") {
                println!("插件 {} 已是最新版本，无需更新。", plugin_id);
            } else {
                anyhow::bail!("更新失败: {e}");
            }
        }
    }
    Ok(())
}

// ── plugin info ─────────────────────────────────────────────────────────

pub fn run_plugin_info(plugin_id: &str) -> Result<()> {
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let installed = peri_middlewares::plugin::config::load_installed_plugins(Some(&plugins_path))
        .unwrap_or_default();

    let target = installed
        .plugins
        .iter()
        .find(|p| p.id == plugin_id || p.name == plugin_id);
    let target = match target {
        Some(p) => p,
        None => {
            anyhow::bail!("未找到插件: {}", plugin_id);
        }
    };

    println!("名称:        {}", target.name);
    println!("ID:          {}", target.id);
    println!("版本:        {}", target.version);
    println!("Marketplace: {}", target.marketplace);
    println!("安装路径:    {}", target.install_path.display());
    println!(
        "Scope:       {}",
        match target.scope {
            peri_middlewares::plugin::InstallScope::User => "user",
            peri_middlewares::plugin::InstallScope::Project => "project",
            peri_middlewares::plugin::InstallScope::Local => "local",
        }
    );

    // 检查是否启用（根据 scope 选择正确的 settings.json 路径）
    let settings_path = match target.scope {
        peri_middlewares::plugin::InstallScope::Project => {
            let cwd = std::env::current_dir().unwrap_or_default();
            cwd.join(".claude").join("settings.json")
        }
        _ => claude_dir.join("settings.json"),
    };
    let enabled = if settings_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&settings_path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&content)
        {
            value
                .get("enabledPlugins")
                .and_then(|ep| {
                    ep.as_object()
                        .map(|obj| obj.contains_key(&target.id))
                        .or_else(|| {
                            ep.as_array().map(|arr| {
                                arr.iter()
                                    .any(|v| v.as_str().is_some_and(|s| s == target.id))
                            })
                        })
                })
                .unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    };
    println!("已启用:      {}", if enabled { "是" } else { "否" });

    Ok(())
}

// ── plugin cleanup ──────────────────────────────────────────────────────

pub async fn run_plugin_cleanup(claude_dir: &Path) -> Result<()> {
    let count = peri_middlewares::plugin::cleanup_orphaned_plugins(claude_dir)
        .await
        .map_err(|e| anyhow::anyhow!("清理失败: {e}"))?;

    if count > 0 {
        println!("已清理 {} 个孤儿插件文件。", count);
    } else {
        println!("没有需要清理的孤儿插件文件。");
    }
    Ok(())
}

// ── plugin search ────────────────────────────────────────────────────────

pub fn run_plugin_search(query: &str) -> Result<()> {
    let cache_dir = peri_middlewares::plugin::config::marketplaces_cache_dir();
    let query_lower = query.to_lowercase();
    let mut found = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let mp_dir = entry.path();
            if !mp_dir.is_dir() {
                continue;
            }
            let mp_name = mp_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let manifest_path = mp_dir.join("marketplace.json");
            if let Ok(content) = std::fs::read_to_string(&manifest_path)
                && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(plugins) = manifest.get("plugins").and_then(|v| v.as_array())
            {
                for p in plugins {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let desc = p.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("");
                    if name.to_lowercase().contains(&query_lower)
                        || desc.to_lowercase().contains(&query_lower)
                    {
                        found.push((
                            name.to_string(),
                            version.to_string(),
                            mp_name.clone(),
                            desc.to_string(),
                        ));
                    }
                }
            }
        }
    }

    if found.is_empty() {
        println!("未找到匹配 \"{}\" 的插件。", query);
    } else {
        println!("{:<40} {:<12} {:<20} 描述", "名称", "版本", "市场");
        println!("{}", "-".repeat(100));
        for (name, version, mp, desc) in &found {
            println!("{:<40} {:<12} {:<20} {}", name, version, mp, desc,);
        }
    }
    Ok(())
}
