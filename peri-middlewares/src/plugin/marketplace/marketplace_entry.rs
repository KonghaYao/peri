use super::{
    fetch, DateTime, Error, MarketplaceManifest, MarketplaceSource, Path, PathBuf, PluginAuthor,
    Utc,
};

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum MarketplaceError {
    #[error("Git 操作失败: {0}")]
    GitFailed(String),
    #[error("HTTP 请求失败: {0}")]
    HttpFailed(String),
    #[error("JSON 解析失败: {0}")]
    ParseFailed(String),
    #[error("marketplace.json 未找到: {path}")]
    ManifestNotFound { path: String },
    #[error("NPM 操作失败: {0}")]
    NpmFailed(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarketplaceStatus {
    Cached,
    Fetching,
    Fresh,
    Stale(String),
    NotFetched,
}

#[derive(Debug)]
pub struct MarketplaceEntry {
    pub name: String,
    pub source: MarketplaceSource,
    pub manifest: Option<MarketplaceManifest>,
    pub status: MarketplaceStatus,
    pub last_updated: Option<DateTime<Utc>>,
    pub auto_update: bool,
}

#[derive(Debug, Clone)]
pub struct AvailablePlugin {
    pub name: String,
    pub description: String,
    pub version: String,
    pub marketplace: String,
    pub source: serde_json::Value,
    pub author: Option<PluginAuthor>,
    pub category: Option<String>,
    pub homepage: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MarketplaceRefreshEvent {
    Updated {
        index: usize,
        name: String,
    },
    Failed {
        index: usize,
        name: String,
        error: String,
    },
}

// ─── Utility Functions ────────────────────────────────────────────────

pub fn find_marketplace_json(dir: &Path) -> Option<PathBuf> {
    let root = dir.join("marketplace.json");
    if root.exists() {
        return Some(root);
    }
    let subdir = dir.join(".claude-plugin").join("marketplace.json");
    if subdir.exists() {
        return Some(subdir);
    }
    None
}

pub fn read_manifest_from_path(path: &Path) -> Result<MarketplaceManifest, MarketplaceError> {
    let content = std::fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|e| MarketplaceError::ParseFailed(e.to_string()))
}

// ─── Parse & Refresh ──────────────────────────────────────────────────

/// 解析用户输入的 marketplace source 字符串
pub fn parse_marketplace_input(input: &str) -> Result<MarketplaceSource, String> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Err("输入不能为空".to_string());
    }

    // 1. Git SSH URLs: user@host:path 或 user@host:path.git
    if let Some(ssh_match) = trimmed.strip_prefix("git@") {
        if let Some((host, path)) = ssh_match.split_once(':') {
            let path = path.strip_suffix(".git").unwrap_or(path);
            return Ok(MarketplaceSource::GitHub {
                repo: format!("git@{}:{}", host, path),
            });
        }
    }

    // 2. HTTP/HTTPS URLs
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if trimmed.contains("github.com/") {
            let parts: Vec<&str> = trimmed.split('/').collect();
            if parts.len() >= 5 {
                let owner = parts[3];
                let repo = parts[4].trim_end_matches(".git");
                return Ok(MarketplaceSource::GitHub {
                    repo: format!("{}/{}", owner, repo),
                });
            }
        }
        return Ok(MarketplaceSource::Url {
            url: trimmed.to_string(),
        });
    }

    // 3. 本地路径：./, ../, /, ~ 开头
    if trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('/')
        || trimmed.starts_with('~')
        || trimmed.starts_with(".\\")
        || trimmed.starts_with("..\\")
        || (trimmed.len() >= 3 && trimmed.as_bytes()[1] == b'\\')
        || (trimmed.len() >= 2
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && trimmed.as_bytes()[1] == b':')
    {
        let path = shellexpand::tilde(trimmed).to_string();
        let path_obj = Path::new(&path);
        if path_obj.ends_with(".json") || path_obj.extension().is_some_and(|e| e == "json") {
            return Ok(MarketplaceSource::File { path });
        } else {
            return Ok(MarketplaceSource::Directory { path });
        }
    }

    // 4. GitHub shorthand: owner/repo
    if trimmed.contains('/') && !trimmed.starts_with('@') {
        let parts: Vec<&str> = trimmed.split('/').collect();
        if parts.len() == 2 {
            return Ok(MarketplaceSource::GitHub {
                repo: trimmed.to_string(),
            });
        }
    }

    // 5. NPM package: @scope/name 或 name
    if trimmed.starts_with('@') || !trimmed.contains('/') {
        return Ok(MarketplaceSource::Npm {
            package: trimmed.to_string(),
        });
    }

    Err(format!("无法识别的 marketplace source: {}", trimmed))
}

/// 刷新单个 marketplace 的缓存，返回 manifest 和缓存路径
pub async fn refresh_marketplace(
    source: &MarketplaceSource,
    name: &str,
) -> Result<(MarketplaceManifest, String), MarketplaceError> {
    let cache_base = crate::plugin::config::marketplaces_cache_dir();
    let auto_update = true;

    let manifest = match source {
        MarketplaceSource::GitHub { repo } => {
            fetch::fetch_github(name, repo, &cache_base, auto_update).await?
        }
        MarketplaceSource::Git { url } => {
            fetch::fetch_git(name, url, &cache_base, auto_update).await?
        }
        MarketplaceSource::Url { url } => fetch::fetch_url(name, url, &cache_base).await?,
        MarketplaceSource::File { path } => {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fetch::read_file(Path::new(&path)))
                .await
                .expect("spawn_blocking panicked")?
        }
        MarketplaceSource::Directory { path } => {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fetch::read_directory(Path::new(&path)))
                .await
                .expect("spawn_blocking panicked")?
        }
        MarketplaceSource::Npm { package } => fetch::fetch_npm(name, package, &cache_base).await?,
    };

    let install_location = match source {
        MarketplaceSource::GitHub { .. }
        | MarketplaceSource::Git { .. }
        | MarketplaceSource::Npm { .. } => cache_base.join(name).display().to_string(),
        MarketplaceSource::Url { .. } => cache_base
            .join(format!("{name}.json"))
            .display()
            .to_string(),
        MarketplaceSource::File { path } => path.clone(),
        MarketplaceSource::Directory { path } => path.clone(),
    };

    Ok((manifest, install_location))
}
