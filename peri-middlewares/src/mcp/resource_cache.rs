//! 持久化 MCP Resource Cache。
//!
//! 仅将 server 明确标记为 `cacheScope: public` 的响应写入磁盘；private
//! 响应继续由 rmcp 的连接内缓存管理，绝不跨进程持久化。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use peri_acp_types::plugin::McpServerConfig;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CACHE_NAMESPACE: &str = "peri:mcp-resource-cache:v2";
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct McpResourceCache {
    path: PathBuf,
}

#[derive(Clone)]
pub(crate) struct CacheTicket {
    origin: String,
    method: &'static str,
    params: String,
    method_epoch: String,
    entry_epoch: String,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    origin: String,
    method_epoch: String,
    entry_epoch: String,
    expires_at_ms: u128,
    payload: serde_json::Value,
}

impl McpResourceCache {
    pub(crate) fn new() -> Self {
        let base = dirs_next::home_dir().unwrap_or_else(std::env::temp_dir);
        Self {
            path: base.join(".peri").join("cache").join("mcp"),
        }
    }

    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
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
                tracing::warn!(origin, method, %error, "MCP 磁盘缓存条目无法解析，删除");
                self.remove(&key).await;
                return None;
            }
        };
        let ticket = self.ticket(origin, method, params).await;
        if entry.origin != origin
            || entry.expires_at_ms <= now_ms()
            || entry.method_epoch != ticket.method_epoch
            || entry.entry_epoch != ticket.entry_epoch
        {
            self.remove(&key).await;
            return None;
        }
        match serde_json::from_value(entry.payload) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(origin, method, %error, "MCP 磁盘缓存响应无法解析，删除");
                self.remove(&key).await;
                None
            }
        }
    }

    /// 在发起 RPC 前捕获版本。通知若在 RPC 期间到达，之后的 `put` 将拒绝旧响应。
    pub(crate) async fn ticket(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> CacheTicket {
        CacheTicket {
            origin: origin.to_string(),
            method,
            params: params.to_string(),
            method_epoch: self.epoch(origin, method, None).await,
            entry_epoch: self.epoch(origin, method, Some(params)).await,
        }
    }

    #[cfg(test)]
    pub(crate) async fn put<T: Serialize>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
        ttl: Duration,
        response: &T,
    ) {
        let ticket = self.ticket(origin, method, params).await;
        self.put_ticket(&ticket, ttl, response).await;
    }

    pub(crate) async fn put_ticket<T: Serialize>(
        &self,
        ticket: &CacheTicket,
        ttl: Duration,
        response: &T,
    ) {
        if ttl.is_zero() {
            return;
        }
        let payload = match serde_json::to_value(response) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(origin = %ticket.origin, method = ticket.method, %error, "MCP 响应无法序列化到磁盘缓存");
                return;
            }
        };
        let entry = CacheEntry {
            origin: ticket.origin.clone(),
            method_epoch: ticket.method_epoch.clone(),
            entry_epoch: ticket.entry_epoch.clone(),
            expires_at_ms: now_ms().saturating_add(ttl.as_millis()),
            payload,
        };
        let bytes = match serde_json::to_vec(&entry) {
            Ok(bytes) if bytes.len() <= MAX_ENTRY_BYTES => bytes,
            Ok(bytes) => {
                tracing::warn!(origin = %ticket.origin, method = ticket.method, size = bytes.len(), limit = MAX_ENTRY_BYTES, "MCP 响应超过持久化缓存单条上限，跳过");
                return;
            }
            Err(error) => {
                tracing::warn!(origin = %ticket.origin, method = ticket.method, %error, "MCP 磁盘缓存封装无法序列化");
                return;
            }
        };
        let current = self
            .ticket(&ticket.origin, ticket.method, &ticket.params)
            .await;
        if current.method_epoch != ticket.method_epoch || current.entry_epoch != ticket.entry_epoch
        {
            tracing::debug!(origin = %ticket.origin, method = ticket.method, "MCP 响应在失效通知前取得，拒绝写入旧缓存");
            return;
        }
        self.trim(bytes.len() as u64).await;
        if let Err(error) = cacache::write(
            &self.path,
            cache_key(&ticket.origin, ticket.method, &ticket.params),
            bytes,
        )
        .await
        {
            tracing::warn!(origin = %ticket.origin, method = ticket.method, %error, "写入 MCP 磁盘缓存失败");
        }
    }

    pub(crate) async fn invalidate(
        &self,
        origin: &str,
        method: &'static str,
        params: Option<&str>,
    ) {
        let epoch_key = epoch_key(origin, method, params);
        if let Err(error) = cacache::write(
            &self.path,
            epoch_key,
            Uuid::new_v4().to_string().into_bytes(),
        )
        .await
        {
            tracing::warn!(origin, method, %error, "MCP 磁盘缓存失效标记写入失败");
        }
        if let Some(params) = params {
            self.remove(&cache_key(origin, method, params)).await;
        }
    }

    async fn epoch(&self, origin: &str, method: &str, params: Option<&str>) -> String {
        cacache::read(&self.path, epoch_key(origin, method, params))
            .await
            .ok()
            .and_then(|value| String::from_utf8(value).ok())
            .unwrap_or_default()
    }

    async fn remove(&self, key: &str) {
        if let Err(error) = cacache::remove(&self.path, key).await {
            tracing::debug!(%error, "MCP 磁盘缓存条目已不存在或无法移除");
        }
    }

    async fn trim(&self, incoming: u64) {
        let path = self.path.clone();
        let victims = tokio::task::spawn_blocking(move || {
            let mut entries = cacache::list_sync(&path)
                .filter_map(Result::ok)
                .filter(|entry| entry.key.starts_with(CACHE_NAMESPACE))
                .collect::<Vec<_>>();
            let mut total = entries.iter().map(|entry| entry.size as u64).sum::<u64>();
            entries.sort_by_key(|entry| entry.time);
            let mut victims = Vec::new();
            while total.saturating_add(incoming) > MAX_CACHE_BYTES {
                let Some(entry) = entries.first() else { break };
                total = total.saturating_sub(entry.size as u64);
                victims.push(entry.key.clone());
                entries.remove(0);
            }
            victims
        })
        .await
        .unwrap_or_default();
        for key in victims {
            self.remove(&key).await;
        }
    }
}

pub(crate) fn cache_origin(server_name: &str, config: Option<&McpServerConfig>) -> String {
    // 永不持久化或记录配置明文。HTTP 以 endpoint 建立身份；stdio 则将稳定、
    // 规范化的 command / args / env / protocol 版本纳入摘要，避免同名 server 串用。
    let identity = match config {
        Some(config) if config.url.is_some() => {
            format!("http\0{}", config.url.as_deref().unwrap_or_default())
        }
        Some(config) => {
            let env = config
                .env
                .as_ref()
                .map(|env| env.iter().collect::<BTreeMap<_, _>>());
            serde_json::to_string(&(
                config.command.as_deref(),
                config.args.as_deref(),
                env,
                config.protocol_version,
            ))
            .unwrap_or_default()
        }
        None => "unknown".to_string(),
    };
    format!(
        "mcp-origin:{}",
        digest(&format!("{server_name}\0{identity}"))
    )
}

fn cache_key(origin: &str, method: &str, params: &str) -> String {
    format!(
        "{CACHE_NAMESPACE}:entry:{}",
        digest(&format!("{origin}\0{method}\0{params}"))
    )
}

fn epoch_key(origin: &str, method: &str, params: Option<&str>) -> String {
    format!(
        "{CACHE_NAMESPACE}:epoch:{}",
        digest(&format!("{origin}\0{method}\0{}", params.unwrap_or("*")))
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
