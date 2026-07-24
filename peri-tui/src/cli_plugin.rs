//! plugin 子命令实现：list / install / uninstall / marketplace add/list/remove

use anyhow::Result;
use chrono::Local;

use crate::cli_args::PluginScope;
use peri_middlewares::plugin::{
    KnownMarketplace, MarketplaceSource, load_known_marketplaces,
    marketplace::{MarketplaceManager, refresh_marketplace},
    parse_marketplace_input, save_known_marketplaces,
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

    for mkt in &marketplaces {
        if MarketplaceManager::extract_name(&mkt.source) == name {
            anyhow::bail!("marketplace \"{}\" 已存在", name);
        }
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

    let filtered: Vec<KnownMarketplace> = marketplaces
        .into_iter()
        .filter(|mkt| MarketplaceManager::extract_name(&mkt.source) != name)
        .collect();

    if filtered.len() == original_len {
        anyhow::bail!("未找到名为 \"{}\" 的 marketplace", name);
    }

    save_known_marketplaces(&filtered, None)
        .map_err(|e| anyhow::anyhow!("保存 marketplace 失败: {e}"))?;

    println!("已删除 marketplace: {}", name);
    Ok(())
}
