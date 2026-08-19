//! 持久化 MCP Resource Cache。
//!
//! 仅将 server 明确标记为 `cacheScope: public` 的响应写入磁盘；private
//! 响应继续由 rmcp 的连接内缓存管理，绝不跨进程持久化。

use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CACHE_NAMESPACE: &str = "peri:mcp-resource-cache:v1";

#[derive(Clone)]
pub(crate) struct McpResourceCache {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    origin: String,
    generation: String,
    expires_at_ms: u128,
    payload: serde_json::Value,
}

impl McpResourceCache {
    pub(crate) fn new() -> Self {
        let base = dirs_next::data_local_dir()
            .or_else(dirs_next::data_dir)
            .unwrap_or_else(std::env::temp_dir);
        Self {
            // 此目录不在任何本地 Skill discovery root 下；即便响应来自本地
            // stdio server，也始终保留为 MCP origin 内容。
            path: base.join("peri").join("cache").join("mcp"),
        }
    }

    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &str,
        params: &str,
    ) -> Option<T> {
        let key = cache_key(origin, method, params);
        let bytes = match cacache::read(&self.path, &key).await {
            Ok(bytes) => bytes,
            Err(_) => return None,
        };
        let entry: CacheEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(origin, method, %error, "MCP 磁盘缓存条目无法解析，忽略");
                return None;
            }
        };
        if entry.origin != origin
            || entry.expires_at_ms <= now_ms()
            || entry.generation != self.generation(origin, method).await
        {
            return None;
        }
        match serde_json::from_value(entry.payload) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(origin, method, %error, "MCP 磁盘缓存响应无法解析，忽略");
                None
            }
        }
    }

    pub(crate) async fn put<T: Serialize>(
        &self,
        origin: &str,
        method: &str,
        params: &str,
        ttl: Duration,
        response: &T,
    ) {
        if ttl.is_zero() {
            return;
        }
        let expires_at_ms = now_ms().saturating_add(ttl.as_millis());
        let payload = match serde_json::to_value(response) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(origin, method, %error, "MCP 响应无法序列化到磁盘缓存");
                return;
            }
        };
        let entry = CacheEntry {
            origin: origin.to_string(),
            generation: self.generation(origin, method).await,
            expires_at_ms,
            payload,
        };
        let bytes = match serde_json::to_vec(&entry) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(origin, method, %error, "MCP 磁盘缓存封装无法序列化");
                return;
            }
        };
        if let Err(error) =
            cacache::write(&self.path, cache_key(origin, method, params), bytes).await
        {
            tracing::warn!(origin, method, %error, "写入 MCP 磁盘缓存失败");
        }
    }

    pub(crate) async fn invalidate(&self, origin: &str, method: &str, params: Option<&str>) {
        if let Some(params) = params {
            if let Err(error) = cacache::remove(&self.path, cache_key(origin, method, params)).await
            {
                tracing::debug!(origin, method, %error, "MCP 磁盘缓存条目已不存在或无法移除");
            }
            return;
        }
        // 列表变化会影响所有分页 cursor，推进 generation 避免遍历磁盘索引。
        if let Err(error) = cacache::write(
            &self.path,
            generation_key(origin, method),
            Uuid::new_v4().to_string().into_bytes(),
        )
        .await
        {
            tracing::warn!(origin, method, %error, "MCP 磁盘缓存失效标记写入失败");
        }
    }

    async fn generation(&self, origin: &str, method: &str) -> String {
        match cacache::read(&self.path, generation_key(origin, method)).await {
            Ok(value) => String::from_utf8(value).unwrap_or_default(),
            Err(_) => String::new(),
        }
    }
}

pub(crate) fn cache_origin(server_name: &str, url: Option<&str>) -> String {
    // 不把 endpoint（它可能含用户信息）原样持久化或记录到日志；该不可逆的
    // 标识仍为 server label + endpoint 的稳定 origin 分区。
    format!(
        "mcp-origin:{}",
        digest(&format!("{server_name}\0{}", url.unwrap_or("stdio")))
    )
}

fn cache_key(origin: &str, method: &str, params: &str) -> String {
    format!(
        "{CACHE_NAMESPACE}:entry:{}",
        digest(&format!("{origin}\0{method}\0{params}"))
    )
}

fn generation_key(origin: &str, method: &str) -> String {
    format!(
        "{CACHE_NAMESPACE}:generation:{}",
        digest(&format!("{origin}\0{method}"))
    )
}

fn digest(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
#[path = "resource_cache_test.rs"]
mod tests;
