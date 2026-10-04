use std::path::PathBuf;

use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::fetch::{fetch_git, fetch_github, fetch_npm, fetch_url, read_directory, read_file};
use super::{
    find_marketplace_json, read_manifest_from_path, AvailablePlugin, MarketplaceEntry,
    MarketplaceRefreshEvent, MarketplaceStatus,
};
use crate::plugin::{
    config::{
        claude_settings_path, ensure_plugin_dirs, known_marketplaces_path, load_claude_settings,
        load_known_marketplaces, marketplaces_cache_dir, save_known_marketplaces,
    },
    types::{KnownMarketplace, MarketplaceManifest, MarketplacePlugin, MarketplaceSource},
};

pub struct MarketplaceManager {
    pub(crate) entries: Vec<MarketplaceEntry>,
    override_dir: Option<PathBuf>,
}

impl MarketplaceManager {
    pub fn new(override_dir: Option<PathBuf>) -> Self {
        MarketplaceManager {
            entries: Vec::new(),
            override_dir,
        }
    }

    fn cache_base(&self) -> PathBuf {
        if let Some(ref dir) = self.override_dir {
            dir.join("marketplaces")
        } else {
            marketplaces_cache_dir()
        }
    }

    fn known_marketplaces_path(&self) -> PathBuf {
        if let Some(ref dir) = self.override_dir {
            dir.join("known_marketplaces.json")
        } else {
            known_marketplaces_path()
        }
    }

    fn claude_settings_path(&self) -> PathBuf {
        if let Some(ref dir) = self.override_dir {
            dir.join("settings.json")
        } else {
            claude_settings_path()
        }
    }

    pub fn try_load_cache(
        &self,
        source: &MarketplaceSource,
        name: &str,
    ) -> Option<MarketplaceManifest> {
        let cache_base = self.cache_base();
        let path: Option<PathBuf> = match source {
            MarketplaceSource::GitHub { .. } => find_marketplace_json(&cache_base.join(name)),
            MarketplaceSource::Git { .. } => find_marketplace_json(&cache_base.join(name)),
            MarketplaceSource::Url { .. } => {
                let p = cache_base.join(format!("{name}.json"));
                if p.exists() {
                    Some(p)
                } else {
                    None
                }
            }
            MarketplaceSource::File { path } => {
                let p = PathBuf::from(path);
                if p.exists() {
                    Some(p)
                } else {
                    None
                }
            }
            MarketplaceSource::Directory { path } => {
                find_marketplace_json(std::path::Path::new(path))
            }
            MarketplaceSource::Npm { .. } => find_marketplace_json(&cache_base.join(name)),
        };
        path.and_then(|p| {
            read_manifest_from_path(&p)
                .map_err(|e| {
                    debug!("缓存 manifest 读取失败 {:?}: {}", p, e);
                    e
                })
                .ok()
        })
    }

    pub fn extract_name(source: &MarketplaceSource) -> String {
        match source {
            MarketplaceSource::GitHub { repo } => {
                repo.split('/').next_back().unwrap_or(repo).to_string()
            }
            MarketplaceSource::Git { url } => {
                if let Some(last) = url.rsplit('/').next() {
                    last.strip_suffix(".git").unwrap_or(last).to_string()
                } else {
                    "git-marketplace".into()
                }
            }
            MarketplaceSource::Url { url } => url::Url::parse(url)
                .ok()
                .and_then(|u| {
                    u.path_segments()
                        .and_then(|mut segs| segs.next_back().map(|s| s.to_string()))
                })
                .map(|s| s.trim_end_matches(".json").to_string())
                .unwrap_or_else(|| "url-marketplace".into()),
            MarketplaceSource::File { path } => PathBuf::from(path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file-marketplace".into()),
            MarketplaceSource::Directory { path } => PathBuf::from(path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "dir-marketplace".into()),
            MarketplaceSource::Npm { package } => package.clone(),
        }
    }

    pub async fn init(
        &mut self,
        tx: mpsc::Sender<MarketplaceRefreshEvent>,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        ensure_plugin_dirs();
        let km_path = self.known_marketplaces_path();
        let settings_path = self.claude_settings_path();
        let mut known = load_known_marketplaces(Some(&km_path)).unwrap_or_default();
        let settings = load_claude_settings(Some(&settings_path)).unwrap_or_default();

        // Merge extra known marketplaces from settings.json
        for extra in &settings.extra_known_marketplaces {
            let extra_json = serde_json::to_string(&extra.source).unwrap_or_default();
            let already_exists = known
                .iter()
                .any(|km| serde_json::to_string(&km.source).unwrap_or_default() == extra_json);
            if !already_exists {
                known.push(KnownMarketplace::from(extra.clone()));
            }
        }

        // Auto-register official marketplace
        let has_official = known.iter().any(|km| match &km.source {
            MarketplaceSource::GitHub { repo } => repo == "anthropics/claude-plugins-official",
            _ => false,
        });
        if !has_official {
            let official = KnownMarketplace {
                source: MarketplaceSource::GitHub {
                    repo: "anthropics/claude-plugins-official".into(),
                },
                install_location: String::new(),
                auto_update: true,
                last_updated: String::new(),
            };
            known.push(official);
            let _ = save_known_marketplaces(&known, Some(&km_path));
        }

        // Build entries
        self.entries.clear();
        for km in &known {
            let name = Self::extract_name(&km.source);
            let cached_manifest = self.try_load_cache(&km.source, &name);
            let status = if cached_manifest.is_some() {
                MarketplaceStatus::Cached
            } else {
                MarketplaceStatus::NotFetched
            };
            let last_updated = chrono::DateTime::parse_from_rfc3339(&km.last_updated)
                .ok()
                .map(|dt| dt.with_timezone(&Utc));
            self.entries.push(MarketplaceEntry {
                name,
                source: km.source.clone(),
                manifest: cached_manifest,
                status,
                last_updated,
                auto_update: km.auto_update,
            });
        }

        // Spawn background refresh tasks
        let mut handles = Vec::new();
        for i in 0..self.entries.len() {
            handles.push(self.spawn_refresh(i, tx.clone()));
        }
        handles
    }

    pub fn spawn_refresh(
        &self,
        index: usize,
        tx: mpsc::Sender<MarketplaceRefreshEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let name = self.entries[index].name.clone();
        let source = self.entries[index].source.clone();
        let auto_update = self.entries[index].auto_update;
        let cache_base = self.cache_base();

        tokio::spawn(async move {
            let result = match &source {
                MarketplaceSource::GitHub { repo } => {
                    fetch_github(&name, repo, &cache_base, auto_update).await
                }
                MarketplaceSource::Git { url } => {
                    fetch_git(&name, url, &cache_base, auto_update).await
                }
                MarketplaceSource::Url { url } => fetch_url(&name, url, &cache_base).await,
                MarketplaceSource::File { path } => {
                    let p = path.clone();
                    tokio::task::spawn_blocking(move || read_file(std::path::Path::new(&p)))
                        .await
                        .expect("spawn_blocking panicked")
                }
                MarketplaceSource::Directory { path } => {
                    let p = path.clone();
                    tokio::task::spawn_blocking(move || read_directory(std::path::Path::new(&p)))
                        .await
                        .expect("spawn_blocking panicked")
                }
                MarketplaceSource::Npm { package } => fetch_npm(&name, package, &cache_base).await,
            };

            match result {
                Ok(manifest) => {
                    let _ = tx
                        .send(MarketplaceRefreshEvent::Updated {
                            index,
                            name: name.clone(),
                        })
                        .await;
                    let _ = manifest; // caller uses event to trigger update
                }
                Err(e) => {
                    warn!("Marketplace '{}' 刷新失败: {}", name, e);
                    let _ = tx
                        .send(MarketplaceRefreshEvent::Failed {
                            index,
                            name: name.clone(),
                            error: e.to_string(),
                        })
                        .await;
                }
            }
        })
    }

    pub fn entries(&self) -> &[MarketplaceEntry] {
        &self.entries
    }

    pub fn update_entry(
        &mut self,
        index: usize,
        manifest: MarketplaceManifest,
        status: MarketplaceStatus,
    ) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.manifest = Some(manifest);
            entry.status = status;
            entry.last_updated = Some(peri_time::now_wall().into());
        }
    }

    pub fn find_plugin(&self, plugin_name: &str) -> Option<(&MarketplacePlugin, &str)> {
        for entry in &self.entries {
            if entry.status != MarketplaceStatus::Cached && entry.status != MarketplaceStatus::Fresh
            {
                continue;
            }
            if let Some(ref manifest) = entry.manifest {
                for plugin in &manifest.plugins {
                    if plugin.name == plugin_name {
                        return Some((plugin, &entry.name));
                    }
                }
            }
        }
        None
    }

    pub fn available_plugins(&self) -> Vec<AvailablePlugin> {
        let mut result = Vec::new();
        for entry in &self.entries {
            if entry.status != MarketplaceStatus::Cached && entry.status != MarketplaceStatus::Fresh
            {
                continue;
            }
            if let Some(ref manifest) = entry.manifest {
                for p in &manifest.plugins {
                    result.push(AvailablePlugin {
                        name: p.name.clone(),
                        description: p.description.clone(),
                        version: p.version.clone(),
                        marketplace: entry.name.clone(),
                        source: p.source.clone(),
                        author: p.author.clone(),
                        category: p.category.clone(),
                        homepage: p.homepage.clone(),
                    });
                }
            }
        }
        result
    }
}
