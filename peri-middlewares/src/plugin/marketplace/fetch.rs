use std::path::Path;

use chrono::{DateTime, Utc};
use tracing::warn;

use super::{find_marketplace_json, read_manifest_from_path, MarketplaceError};
use crate::plugin::types::MarketplaceManifest;

pub(crate) async fn fetch_github(
    name: &str,
    repo: &str,
    cache_base: &Path,
    auto_update: bool,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let url = format!("https://github.com/{repo}.git");
    fetch_git(name, &url, cache_base, auto_update).await
}

/// 通用的 git 仓库（任意 git URL）
pub(crate) async fn fetch_git(
    name: &str,
    url: &str,
    cache_base: &Path,
    auto_update: bool,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let cache_dir = cache_base.join(name);

    if !cache_dir.exists() {
        let mut command = tokio::process::Command::new("git");
        command.args([
            "clone",
            "--depth",
            "1",
            url,
            &cache_dir.display().to_string(),
        ]);
        let output = peri_time::timeout(
            std::time::Duration::from_secs(30),
            peri_process::run_output(command),
        )
        .await
        .map_err(|e| MarketplaceError::GitFailed(format!("clone 超时: {e}")))?
        .map_err(|e| MarketplaceError::GitFailed(format!("clone 执行失败: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MarketplaceError::GitFailed(format!("clone 失败: {stderr}")));
        }
    } else if auto_update {
        let mut command = tokio::process::Command::new("git");
        command.args(["-C", &cache_dir.display().to_string(), "pull", "--ff-only"]);
        let output = peri_time::timeout(
            std::time::Duration::from_secs(30),
            peri_process::run_output(command),
        )
        .await
        .map_err(|e| MarketplaceError::GitFailed(format!("pull 超时: {e}")))?
        .map_err(|e| MarketplaceError::GitFailed(format!("pull 执行失败: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("git pull 失败 '{}', 回退到缓存: {stderr}", url);
            // fall through to read cache
        }
    }

    let manifest_path =
        find_marketplace_json(&cache_dir).ok_or_else(|| MarketplaceError::ManifestNotFound {
            path: cache_dir.display().to_string(),
        })?;
    read_manifest_from_path(&manifest_path)
}

pub(crate) async fn fetch_url(
    name: &str,
    url: &str,
    cache_base: &Path,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let cache_file = cache_base.join(format!("{name}.json"));

    let last_modified = std::fs::metadata(&cache_file)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
        });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| MarketplaceError::HttpFailed(e.to_string()))?;

    let mut req = client.get(url);
    if let Some(ref lm) = last_modified {
        req = req.header("If-Modified-Since", lm);
    }

    let result = req.send().await;

    match result {
        Ok(response) => match response.status().as_u16() {
            304 => read_manifest_from_path(&cache_file),
            200 => {
                let body = response
                    .text()
                    .await
                    .map_err(|e| MarketplaceError::HttpFailed(e.to_string()))?;
                if let Some(parent) = cache_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&cache_file, &body)?;
                serde_json::from_str(&body)
                    .map_err(|e| MarketplaceError::ParseFailed(e.to_string()))
            }
            status => Err(MarketplaceError::HttpFailed(format!("HTTP {status}"))),
        },
        Err(e) => {
            if cache_file.exists() {
                warn!("URL 拉取失败 '{}', 回退到缓存: {}", url, e);
                read_manifest_from_path(&cache_file)
            } else {
                Err(MarketplaceError::HttpFailed(e.to_string()))
            }
        }
    }
}

pub(crate) fn read_file(path: &Path) -> Result<MarketplaceManifest, MarketplaceError> {
    read_manifest_from_path(path)
}

pub(crate) fn read_directory(path: &Path) -> Result<MarketplaceManifest, MarketplaceError> {
    let manifest_path =
        find_marketplace_json(path).ok_or_else(|| MarketplaceError::ManifestNotFound {
            path: path.display().to_string(),
        })?;
    read_manifest_from_path(&manifest_path)
}

pub(crate) async fn fetch_npm(
    name: &str,
    package: &str,
    cache_base: &Path,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let cache_dir = cache_base.join(name);

    if let Some(manifest_path) = find_marketplace_json(&cache_dir) {
        return read_manifest_from_path(&manifest_path);
    }

    let tmp_dir = std::env::temp_dir().join(format!("npm-pack-{package}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir)?;
    let mut command = tokio::process::Command::new("npm");
    command.args([
        "pack",
        package,
        "--pack-destination",
        &tmp_dir.display().to_string(),
    ]);
    let output = peri_time::timeout(
        std::time::Duration::from_secs(60),
        peri_process::run_output(command),
    )
    .await
    .map_err(|e| MarketplaceError::NpmFailed(format!("npm pack 超时: {e}")))?
    .map_err(|e| MarketplaceError::NpmFailed(format!("npm pack 执行失败: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MarketplaceError::NpmFailed(format!(
            "npm pack 失败: {stderr}"
        )));
    }

    let tgz_path = std::fs::read_dir(&tmp_dir)?
        .find_map(|e| {
            e.ok().and_then(|f| {
                if f.path()
                    .extension()
                    .map(|ext| ext == "tgz")
                    .unwrap_or(false)
                {
                    Some(f.path())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| MarketplaceError::NpmFailed("未找到 .tgz 文件".into()))?;

    let file = std::fs::File::open(&tgz_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    std::fs::create_dir_all(&cache_dir)?;
    archive.unpack(&cache_dir)?;

    let manifest_path =
        find_marketplace_json(&cache_dir).ok_or_else(|| MarketplaceError::ManifestNotFound {
            path: cache_dir.display().to_string(),
        })?;
    read_manifest_from_path(&manifest_path)
}
