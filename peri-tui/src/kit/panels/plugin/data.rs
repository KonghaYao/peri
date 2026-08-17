use std::sync::OnceLock;

use super::{MsEntry, MsStatus, PluginSearchResultItem};

// ── Discover cache (non-reactive, safe in render body) ────────────────

/// Discover 插件列表缓存——避免 render body 中同步读盘。
/// 使用 Option<Vec<T>>：None = 未初始化，Some(vec) = 已填充（可能为空）。
/// 避免用 is_empty() 判断初始化状态——空数据集与未初始化无法区分。
static DISCOVER_CACHE: OnceLock<parking_lot::Mutex<Option<Vec<PluginSearchResultItem>>>> =
    OnceLock::new();

pub(super) fn get_discover_cache() -> Vec<PluginSearchResultItem> {
    let cache = DISCOVER_CACHE.get_or_init(|| parking_lot::Mutex::new(None));
    {
        let guard = cache.lock();
        if let Some(ref data) = *guard {
            return data.clone();
        }
    }
    // 锁外执行磁盘 I/O，避免 render body 阻塞在锁上
    let data = load_discover_plugins_from_disk();
    let mut guard = cache.lock();
    if guard.is_none() {
        *guard = Some(data);
    }
    guard.as_ref().unwrap().clone()
}

pub(super) fn refresh_discover_cache() {
    // 先在锁外执行磁盘 I/O，再拿锁替换数据
    let data = load_discover_plugins_from_disk();
    if let Some(cache) = DISCOVER_CACHE.get() {
        let mut guard = cache.lock();
        *guard = Some(data);
    }
}

// ── Marketplace cache (non-reactive, safe in render body) ─────────────

/// Marketplace 数据缓存——避免 render_marketplaces 每帧同步读盘。
/// 使用 Option<Vec<T>>：None = 未初始化，Some(vec) = 已填充（可能为空）。
static MARKETPLACE_CACHE: OnceLock<parking_lot::Mutex<Option<Vec<MsEntry>>>> = OnceLock::new();

pub(super) fn get_marketplace_cache() -> Vec<MsEntry> {
    let cache = MARKETPLACE_CACHE.get_or_init(|| parking_lot::Mutex::new(None));
    {
        let guard = cache.lock();
        if let Some(ref data) = *guard {
            return data.clone();
        }
    }
    // 锁外执行磁盘 I/O，避免 render body 阻塞在锁上
    let data = load_marketplace_data();
    let mut guard = cache.lock();
    if guard.is_none() {
        *guard = Some(data);
    }
    guard.as_ref().unwrap().clone()
}

pub(super) fn refresh_marketplace_cache() {
    // 先在锁外执行磁盘 I/O，再拿锁替换数据
    let data = load_marketplace_data();
    if let Some(cache) = MARKETPLACE_CACHE.get() {
        let mut guard = cache.lock();
        *guard = Some(data);
    }
}

fn load_marketplace_data() -> Vec<MsEntry> {
    let known = peri_middlewares::plugin::load_known_marketplaces(None).unwrap_or_default();
    let cache_dir = peri_middlewares::plugin::marketplaces_cache_dir();
    let _ = std::fs::create_dir_all(&cache_dir);

    let installed = peri_middlewares::plugin::load_installed_plugins(None).unwrap_or_default();

    known
        .iter()
        .map(|km| {
            let name = peri_middlewares::plugin::MarketplaceManager::extract_name(&km.source);

            // 确定状态
            let cache_path = cache_dir.join(&name);
            let manifest_path =
                peri_middlewares::plugin::marketplace::find_marketplace_json(&cache_path);
            let mut status = if km.install_location.is_empty() {
                MsStatus::NotFound
            } else if manifest_path.is_none() {
                MsStatus::NotFound
            } else {
                MsStatus::Cached
            };

            // B3: 检查 manifest mtime，超过 24h 标记为 Stale
            if status == MsStatus::Cached
                && let Some(ref path) = manifest_path
                && let Ok(meta) = std::fs::metadata(path)
                && let Ok(mtime) = meta.modified()
                && let Ok(elapsed) = mtime.elapsed()
                && elapsed.as_secs() > 24 * 3600
            {
                status = MsStatus::Stale;
            }

            // 从 cached manifest 统计插件数
            let mut manifest_parse_failed = false;
            let plugin_count = match manifest_path.as_ref() {
                Some(path) => {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                            manifest
                                .get("plugins")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0)
                        } else {
                            manifest_parse_failed = true;
                            0
                        }
                    } else {
                        manifest_parse_failed = true;
                        0
                    }
                }
                None => 0,
            };

            if manifest_parse_failed {
                status = MsStatus::Failed;
            }

            // 统计已安装的插件数（来自此 marketplace）
            let installed_count = installed
                .plugins
                .iter()
                .filter(|p| {
                    let mp = if let Some((_, mkt)) = p.id.split_once('@') {
                        mkt
                    } else {
                        ""
                    };
                    mp == name
                })
                .count();

            MsEntry {
                name,
                source_label: match &km.source {
                    peri_middlewares::plugin::MarketplaceSource::GitHub { repo } => {
                        format!("github:{}", repo)
                    }
                    peri_middlewares::plugin::MarketplaceSource::Git { url } => {
                        format!("git:{}", url)
                    }
                    peri_middlewares::plugin::MarketplaceSource::Url { url } => url.clone(),
                    peri_middlewares::plugin::MarketplaceSource::Directory { path } => path.clone(),
                    peri_middlewares::plugin::MarketplaceSource::File { path } => path.clone(),
                    peri_middlewares::plugin::MarketplaceSource::Npm { package } => {
                        format!("npm:{}", package)
                    }
                },
                plugin_count,
                installed_count,
                status,
                last_updated: if km.last_updated.is_empty() {
                    None
                } else {
                    Some(km.last_updated.clone())
                },
                auto_update: km.auto_update,
            }
        })
        .collect()
}

fn load_discover_plugins_from_disk() -> Vec<PluginSearchResultItem> {
    let mut known = peri_middlewares::plugin::load_known_marketplaces(None).unwrap_or_default();
    let cache_dir = peri_middlewares::plugin::marketplaces_cache_dir();
    let _ = std::fs::create_dir_all(&cache_dir);

    // 确保 official marketplace 已注册（参考项目行为：自动注入）
    let has_official = known.iter().any(|km| match &km.source {
        peri_middlewares::plugin::MarketplaceSource::GitHub { repo } => {
            repo == "anthropics/claude-plugins-official"
        }
        _ => false,
    });
    if !has_official {
        known.push(peri_middlewares::plugin::KnownMarketplace {
            source: peri_middlewares::plugin::MarketplaceSource::GitHub {
                repo: "anthropics/claude-plugins-official".into(),
            },
            install_location: cache_dir
                .join("claude-plugins-official")
                .to_string_lossy()
                .to_string(),
            auto_update: true,
            last_updated: String::new(),
        });
    }

    let mut plugins: Vec<PluginSearchResultItem> = Vec::new();
    let installed = peri_middlewares::plugin::load_installed_plugins(None).unwrap_or_default();
    let installed_ids: std::collections::HashSet<String> =
        installed.plugins.iter().map(|p| p.id.clone()).collect();

    for km in &known {
        let mp_name = peri_middlewares::plugin::MarketplaceManager::extract_name(&km.source);
        let mp_dir = cache_dir.join(&mp_name);
        let manifest_path =
            match peri_middlewares::plugin::marketplace::find_marketplace_json(&mp_dir) {
                Some(path) => path,
                None => continue,
            };
        if let Ok(content) = std::fs::read_to_string(&manifest_path)
            && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(plugin_list) = manifest.get("plugins").and_then(|v| v.as_array())
        {
            for p in plugin_list {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let plugin_id = format!("{}@{}", name, mp_name);
                if installed_ids.contains(&plugin_id) {
                    continue;
                }
                // author 可能是字符串或 {"name": "..."} 对象
                let author = p.get("author").and_then(|v| {
                    v.as_str().map(|s| s.to_string()).or_else(|| {
                        v.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                });
                let version = p
                    .get("version")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("—")
                    .to_string();
                plugins.push(PluginSearchResultItem {
                    name,
                    version,
                    marketplace: mp_name.clone(),
                    description: p
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    author,
                });
            }
        }
    }
    plugins
}
