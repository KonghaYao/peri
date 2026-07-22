use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, FixedOffset};

use crate::plugin::config::plugins_dir;

const INSTALL_COUNTS_URL: &str = "https://raw.githubusercontent.com/anthropics/claude-plugins-official/refs/heads/stats/stats/plugin-installs.json";
const CACHE_FILE: &str = "install-counts-cache.json";
const CACHE_TTL_SECS: i64 = 24 * 3600;

/// Claude Code 写入的缓存条目格式
#[derive(Debug, serde::Deserialize)]
struct CachedInstallEntry {
    plugin: String,
    unique_installs: u64,
}

/// Claude Code 缓存文件格式（兼容）
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum CacheFormat {
    /// Claude Code 格式：{version, fetchedAt, counts: [{plugin, unique_installs}]}
    ClaudeCode {
        #[allow(dead_code)]
        version: Option<u64>,
        #[allow(dead_code)]
        fetched_at: Option<String>,
        counts: Vec<CachedInstallEntry>,
    },
}

fn cache_path() -> PathBuf {
    plugins_dir().join(CACHE_FILE)
}

/// 从磁盘加载缓存的安装量数据（兼容 Claude Code 缓存格式）。
/// 缓存过期时仍返回旧数据作为降级。文件不存在或无法解析时返回 None。
pub fn load_install_counts() -> Option<HashMap<String, u64>> {
    let path = cache_path();
    let data = std::fs::read_to_string(&path).ok()?;

    let cache: CacheFormat = serde_json::from_str(&data).ok()?;
    match cache {
        CacheFormat::ClaudeCode { counts, .. } => {
            let map: HashMap<String, u64> = counts
                .into_iter()
                .map(|e| (e.plugin, e.unique_installs))
                .collect();
            if map.is_empty() {
                None
            } else {
                Some(map)
            }
        }
    }
}

/// 检查缓存是否存在且未过期
pub fn is_install_counts_cache_valid() -> bool {
    let path = cache_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    // 提取 fetchedAt 字段检查过期
    let fetched_at = extract_fetched_at(&data);
    if let Some(ts) = fetched_at {
        let elapsed = chrono::Utc::now().signed_duration_since(ts).num_seconds();
        return elapsed <= CACHE_TTL_SECS;
    }

    // 无法解析时间戳，保守认为有效（有数据总比没有好）
    true
}

fn extract_fetched_at(json: &str) -> Option<DateTime<FixedOffset>> {
    // 简单提取 "fetchedAt" 字段
    let val: serde_json::Value = serde_json::from_str(json).ok()?;
    let ts = val.get("fetchedAt")?.as_str()?;
    DateTime::parse_from_rfc3339(ts).ok()
}

/// 从远程 URL 异步获取安装量数据并更新缓存。
/// 远程返回 Claude Code 格式：{version, fetchedAt, counts: [{plugin, unique_installs}]}。
pub async fn fetch_install_counts() -> Option<HashMap<String, u64>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;

    let resp = client.get(INSTALL_COUNTS_URL).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    // 远程返回的是完整 Claude Code 格式，直接写入缓存
    let body = resp.text().await.ok()?;

    // 解析提取 counts
    let cache: CacheFormat = serde_json::from_str(&body).ok()?;
    let counts = match cache {
        CacheFormat::ClaudeCode { ref counts, .. } => counts
            .iter()
            .map(|e| (e.plugin.clone(), e.unique_installs))
            .collect::<HashMap<String, u64>>(),
    };

    // 原样写入缓存（保留 fetchedAt 时间戳）
    let dir = plugins_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(cache_path(), &body);

    Some(counts)
}

/// 格式化安装量数字为人类可读字符串。
///
/// - <1000 → 原数字（如 "42"）
/// - >=1000 → K 后缀（如 "1.2K"）
/// - >=1000000 → M 后缀（如 "1.5M"）
pub fn format_install_count(count: u64) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 999_500 {
        let k = count as f64 / 1000.0;
        let formatted = format!("{:.1}", k);
        if formatted.ends_with(".0") {
            format!("{}K", &formatted[..formatted.len() - 2])
        } else {
            format!("{}K", formatted)
        }
    } else {
        let m = count as f64 / 1_000_000.0;
        let formatted = format!("{:.1}", m);
        if formatted.ends_with(".0") {
            format!("{}M", &formatted[..formatted.len() - 2])
        } else {
            format!("{}M", formatted)
        }
    }
}

#[cfg(test)]
#[path = "install_counts_test.rs"]
mod tests;
