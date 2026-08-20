//! 持久化 MCP cache：只缓存明确可跨进程复用的公开响应。
//!
//! v2 将 `cacache` content 与协调状态分离。网络 RPC 不持锁；短暂的本地
//! `get` / `ticket` / `put` / `invalidate` 临界区由进程内 mutex 与跨进程
//! advisory file lock 串行化，避免失效通知与迟到响应重新激活旧缓存。

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use peri_acp_types::plugin::McpServerConfig;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CACHE_NAMESPACE: &str = "peri:mcp-cache:v2";
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const EPOCH_FILE: &str = "epoch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheLoadStatus {
    Hit,
    LiveFetch,
}

#[derive(Clone)]
pub(crate) struct McpResourceCache {
    content_path: PathBuf,
    state_path: PathBuf,
    mutex: Arc<tokio::sync::Mutex<()>>,
    recent_status: Arc<parking_lot::Mutex<HashMap<String, CacheLoadStatus>>>,
}

#[derive(Clone)]
pub(crate) struct CacheTicket {
    origin: String,
    method: &'static str,
    params: String,
    epoch: u64,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    epoch: u64,
    expires_at_ms: u128,
    payload: serde_json::Value,
}

impl McpResourceCache {
    pub(crate) fn new() -> Self {
        let base = dirs_next::home_dir().unwrap_or_else(std::env::temp_dir);
        Self::from_root(base.join(".peri").join("cache").join("mcp").join("v2"))
    }

    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self::from_root(path)
    }

    fn from_root(root: PathBuf) -> Self {
        Self {
            content_path: root.join("content"),
            state_path: root.join("state"),
            mutex: Arc::new(tokio::sync::Mutex::new(())),
            recent_status: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn recent_status(&self, origin: &str) -> Option<CacheLoadStatus> {
        self.recent_status.lock().get(origin).copied()
    }

    pub(crate) fn mark_live_fetch(&self, origin: &str) {
        self.recent_status
            .lock()
            .insert(origin.to_string(), CacheLoadStatus::LiveFetch);
    }

    fn mark_hit(&self, origin: &str) {
        self.recent_status
            .lock()
            .insert(origin.to_string(), CacheLoadStatus::Hit);
    }

    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<T> {
        let _guard = self.mutex.lock().await;
        let lock = self.lock().await?;
        let epoch = self.read_epoch().await?;
        let key = cache_key(origin, method, params);
        let bytes = cacache::read(&self.content_path, &key).await.ok()?;
        let entry: CacheEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(_) => {
                let _ = cacache::remove(&self.content_path, &key).await;
                drop(lock);
                return None;
            }
        };
        if entry.epoch != epoch || entry.expires_at_ms <= now_ms() {
            let _ = cacache::remove(&self.content_path, &key).await;
            drop(lock);
            return None;
        }
        let value = match serde_json::from_value(entry.payload) {
            Ok(value) => Some(value),
            Err(_) => {
                let _ = cacache::remove(&self.content_path, &key).await;
                None
            }
        };
        drop(lock);
        if value.is_some() {
            self.mark_hit(origin);
        }
        value
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<T> {
        self.get(origin, method, params).await
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
        let ticket = self
            .ticket(origin, method, params)
            .await
            .expect("测试 cache 应可用");
        self.put_ticket(&ticket, ttl, response).await;
    }

    /// 在网络 RPC 前捕获 epoch；网络阶段绝不持有 cache lock。
    pub(crate) async fn ticket(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<CacheTicket> {
        let _guard = self.mutex.lock().await;
        let lock = self.lock().await?;
        let epoch = self.read_epoch().await?;
        drop(lock);
        Some(CacheTicket {
            origin: origin.to_string(),
            method,
            params: params.to_string(),
            epoch,
        })
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
            Err(_) => return,
        };
        let entry = CacheEntry {
            epoch: ticket.epoch,
            expires_at_ms: now_ms().saturating_add(ttl.as_millis()),
            payload,
        };
        let bytes = match serde_json::to_vec(&entry) {
            Ok(bytes) if bytes.len() <= MAX_ENTRY_BYTES => bytes,
            Ok(_) | Err(_) => return,
        };

        let _guard = self.mutex.lock().await;
        let lock = match self.lock().await {
            Some(lock) => lock,
            None => return,
        };
        let Some(epoch) = self.read_epoch().await else {
            return;
        };
        if epoch != ticket.epoch {
            tracing::debug!(method = ticket.method, "MCP 缓存失效后拒绝迟到响应");
            drop(lock);
            return;
        }
        self.trim_locked(bytes.len() as u64).await;
        if let Err(error) = cacache::write(
            &self.content_path,
            cache_key(&ticket.origin, ticket.method, &ticket.params),
            bytes,
        )
        .await
        {
            tracing::warn!(method = ticket.method, %error, "写入 MCP 磁盘缓存失败");
        }
        drop(lock);
    }

    pub(crate) async fn invalidate(
        &self,
        _origin: &str,
        _method: &'static str,
        _params: Option<&str>,
    ) {
        let _guard = self.mutex.lock().await;
        let lock = match self.lock().await {
            Some(lock) => lock,
            None => return,
        };
        let Some(epoch) = self.read_epoch().await else {
            return;
        };
        if let Err(error) = self.write_epoch(epoch.saturating_add(1)).await {
            tracing::warn!(%error, "MCP 磁盘缓存失效标记写入失败，已停用本次失效");
        }
        drop(lock);
    }

    async fn lock(&self) -> Option<std::fs::File> {
        let path = self.state_path.join("cache.lock");
        let file = tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)?;
            file.lock()?;
            Ok::<_, std::io::Error>(file)
        })
        .await
        .ok()?
        .ok()?;
        Some(file)
    }

    async fn read_epoch(&self) -> Option<u64> {
        let path = self.state_path.join(EPOCH_FILE);
        match tokio::fs::read(&path).await {
            Ok(bytes) => std::str::from_utf8(&bytes).ok()?.trim().parse().ok(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if tokio::fs::try_exists(&self.content_path).await.ok()? {
                    let mut read_dir = tokio::fs::read_dir(&self.content_path).await.ok()?;
                    if read_dir.next_entry().await.ok()?.is_some() {
                        return None;
                    }
                }
                self.write_epoch(0).await.ok()?;
                Some(0)
            }
            Err(_) => None,
        }
    }

    async fn write_epoch(&self, epoch: u64) -> Result<(), std::io::Error> {
        tokio::fs::create_dir_all(&self.state_path).await?;
        let tmp = self.state_path.join(format!("{EPOCH_FILE}.tmp"));
        tokio::fs::write(&tmp, epoch.to_string()).await?;
        tokio::fs::rename(tmp, self.state_path.join(EPOCH_FILE)).await
    }

    async fn trim_locked(&self, incoming: u64) {
        let content_path = self.content_path.clone();
        let victims = tokio::task::spawn_blocking(move || {
            let mut entries = cacache::list_sync(&content_path)
                .filter_map(Result::ok)
                .filter(|entry| entry.key.starts_with(CACHE_NAMESPACE))
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.time);
            let mut total = entries.iter().map(|entry| entry.size as u64).sum::<u64>();
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
            let _ = cacache::remove(&self.content_path, key).await;
        }
    }
}

pub(crate) fn cache_origin(server_name: &str, config: Option<&McpServerConfig>) -> String {
    // 传输选择以 stdio 优先；command + url 同存时，未使用的 url 不影响 identity。
    let identity = match config {
        Some(config) if config.command.is_none() && config.url.is_some() => {
            format!("http\0{}", config.url.as_deref().unwrap_or_default())
        }
        Some(config) => {
            let env = config
                .env
                .as_ref()
                .map(|env| env.iter().collect::<BTreeMap<_, _>>());
            serde_json::to_string(&(
                "stdio",
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
        "{CACHE_NAMESPACE}:{}",
        digest(&format!("{origin}\0{method}\0{params}"))
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
