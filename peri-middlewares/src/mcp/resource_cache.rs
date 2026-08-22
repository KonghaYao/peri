//! 持久化 MCP cache：仅在响应 scope 与当前授权上下文允许时跨进程复用。
//! private 响应不代表永不持久化；匿名、无 Authorization 的 endpoint 可在
//! 匹配的 server cache version 下复用，OAuth/凭据上下文则由 client policy 禁止。
//!
//! v2 将 `cacache` content 与协调状态分离。网络 RPC 不持锁；短暂的本地
//! `get` / `ticket` / `put` / `invalidate` 临界区由进程内 mutex 与跨进程
//! advisory file lock 串行化，避免失效通知与迟到响应重新激活旧缓存。

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    path::PathBuf,
    sync::{Arc, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use peri_acp_types::plugin::McpServerConfig;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CACHE_NAMESPACE: &str = "peri:mcp-cache:v2";
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const EPOCH_DIR: &str = "epochs";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheLoadStatus {
    VersionHit,
    McppHit,
    ResourceHit,
    LiveFetch,
    StoredAfterFetch,
}

type SharedStatuses = parking_lot::Mutex<HashMap<String, CacheLoadStatus>>;
type SharedVersions = parking_lot::RwLock<HashMap<String, String>>;
type StatusRegistry = parking_lot::Mutex<HashMap<PathBuf, Weak<SharedStatuses>>>;
type VersionRegistry = parking_lot::Mutex<HashMap<PathBuf, Weak<SharedVersions>>>;

fn shared_statuses(root: &PathBuf) -> Arc<SharedStatuses> {
    static REGISTRY: OnceLock<StatusRegistry> = OnceLock::new();
    let registry = REGISTRY.get_or_init(Default::default);
    let mut entries = registry.lock();
    entries.retain(|_, statuses| statuses.strong_count() > 0);
    if let Some(statuses) = entries.get(root).and_then(Weak::upgrade) {
        return statuses;
    }
    let statuses = Arc::new(parking_lot::Mutex::new(HashMap::new()));
    entries.insert(root.clone(), Arc::downgrade(&statuses));
    statuses
}

fn shared_versions(root: &PathBuf) -> Arc<SharedVersions> {
    static REGISTRY: OnceLock<VersionRegistry> = OnceLock::new();
    let registry = REGISTRY.get_or_init(Default::default);
    let mut entries = registry.lock();
    entries.retain(|_, versions| versions.strong_count() > 0);
    if let Some(versions) = entries.get(root).and_then(Weak::upgrade) {
        return versions;
    }
    let versions = Arc::new(parking_lot::RwLock::new(HashMap::new()));
    entries.insert(root.clone(), Arc::downgrade(&versions));
    versions
}

#[derive(Clone)]
pub(crate) struct McpResourceCache {
    content_path: PathBuf,
    state_path: PathBuf,
    mutex: Arc<tokio::sync::Mutex<()>>,
    recent_status: Arc<SharedStatuses>,
    active_versions: Arc<SharedVersions>,
    #[cfg(test)]
    test_root: Option<Arc<tempfile::TempDir>>,
}

#[derive(Clone)]
pub(crate) struct CacheTicket {
    pub(crate) origin: String,
    method: &'static str,
    params: String,
    epoch: u64,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    epoch: u64,
    expires_at_ms: u128,
    #[serde(default)]
    cache_version: Option<String>,
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

    /// 为 pool 测试提供独立的缓存根，避免相同 server/URI 的测试命中彼此
    /// 留下的持久化响应。目录的生命周期与缓存实例绑定。
    #[cfg(test)]
    pub(crate) fn isolated_for_test() -> Self {
        let test_root = Arc::new(tempfile::tempdir().expect("创建 MCP 测试缓存目录失败"));
        let mut cache = Self::from_root(test_root.path().to_path_buf());
        cache.test_root = Some(test_root);
        cache
    }

    fn from_root(root: PathBuf) -> Self {
        let recent_status = shared_statuses(&root);
        let active_versions = shared_versions(&root);
        Self {
            content_path: root.join("content"),
            state_path: root.join("state"),
            mutex: Arc::new(tokio::sync::Mutex::new(())),
            recent_status,
            active_versions,
            #[cfg(test)]
            test_root: None,
        }
    }

    pub(crate) fn recent_status(&self, origin: &str) -> Option<CacheLoadStatus> {
        let statuses = self.recent_status.lock();
        ["skills/", "tools/", "resources/"]
            .into_iter()
            .filter_map(|domain| statuses.get(&status_key(origin, domain)).copied())
            .max_by_key(|status| match status {
                CacheLoadStatus::VersionHit => 3,
                CacheLoadStatus::McppHit => 2,
                CacheLoadStatus::ResourceHit => 1,
                CacheLoadStatus::LiveFetch => 0,
                CacheLoadStatus::StoredAfterFetch => 0,
            })
    }

    pub(crate) fn mark_live_fetch(&self, origin: &str, method: &'static str) {
        let mut statuses = self.recent_status.lock();
        let key = status_key(origin, method);
        statuses.insert(key, CacheLoadStatus::LiveFetch);
    }

    fn mark_hit(&self, origin: &str, method: &'static str, version_fresh: bool) {
        let status = if version_fresh {
            CacheLoadStatus::VersionHit
        } else if method.starts_with("skills/") {
            CacheLoadStatus::McppHit
        } else {
            CacheLoadStatus::ResourceHit
        };
        self.recent_status
            .lock()
            .insert(status_key(origin, method), status);
    }

    fn mark_stored_after_fetch(&self, origin: &str, method: &'static str) {
        self.recent_status.lock().insert(
            status_key(origin, method),
            CacheLoadStatus::StoredAfterFetch,
        );
    }
    pub(crate) fn set_cache_version(&self, origin: &str, cache_version: Option<&str>) {
        let mut versions = self.active_versions.write();
        match cache_version {
            Some(version) => {
                versions.insert(origin.to_string(), version.to_string());
            }
            None => {
                versions.remove(origin);
            }
        }
    }

    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
    ) -> Option<T> {
        let cache_version = self.active_versions.read().get(origin).cloned();
        self.get_versioned(origin, method, params, cache_version.as_deref())
            .await
    }

    pub(crate) async fn get_versioned<T: DeserializeOwned>(
        &self,
        origin: &str,
        method: &'static str,
        params: &str,
        cache_version: Option<&str>,
    ) -> Option<T> {
        let _guard = self.mutex.lock().await;
        let lock = self.lock().await?;
        let epoch = self.read_epoch(origin, method, params).await?;
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
        let version_fresh =
            cache_version.is_some() && entry.cache_version.as_deref() == cache_version;
        let version_mismatch =
            cache_version.is_some() && entry.cache_version.as_deref() != cache_version;
        if entry.epoch != epoch
            || version_mismatch
            || (!version_fresh && entry.expires_at_ms <= now_ms())
        {
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
            self.mark_hit(origin, method, version_fresh);
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
        self.put_ticket_versioned(&ticket, ttl, None, response)
            .await;
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
        let epoch = self.read_epoch(origin, method, params).await?;
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
        let cache_version = self.active_versions.read().get(&ticket.origin).cloned();
        self.put_ticket_versioned(ticket, ttl, cache_version.as_deref(), response)
            .await;
    }

    pub(crate) async fn put_ticket_versioned<T: Serialize>(
        &self,
        ticket: &CacheTicket,
        ttl: Duration,
        cache_version: Option<&str>,
        response: &T,
    ) {
        if ttl.is_zero() && cache_version.is_none() {
            return;
        }
        let payload = match serde_json::to_value(response) {
            Ok(payload) => payload,
            Err(_) => return,
        };
        let entry = CacheEntry {
            epoch: ticket.epoch,
            expires_at_ms: now_ms().saturating_add(ttl.as_millis()),
            cache_version: cache_version.map(str::to_owned),
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
        let Some(epoch) = self
            .read_epoch(&ticket.origin, ticket.method, &ticket.params)
            .await
        else {
            return;
        };
        if epoch != ticket.epoch {
            tracing::debug!(method = ticket.method, "MCP 缓存失效后拒绝迟到响应");
            drop(lock);
            return;
        }
        self.trim_locked(bytes.len() as u64).await;
        match cacache::write(
            &self.content_path,
            cache_key(&ticket.origin, ticket.method, &ticket.params),
            bytes,
        )
        .await
        {
            Ok(_) => self.mark_stored_after_fetch(&ticket.origin, ticket.method),
            Err(error) => {
                tracing::warn!(method = ticket.method, %error, "写入 MCP 磁盘缓存失败");
            }
        }
        drop(lock);
    }

    pub(crate) async fn invalidate(
        &self,
        origin: &str,
        method: &'static str,
        params: Option<&str>,
    ) {
        let params = params.unwrap_or_default();
        let _guard = self.mutex.lock().await;
        let lock = match self.lock().await {
            Some(lock) => lock,
            None => return,
        };
        let Some(epoch) = self.read_epoch(origin, method, params).await else {
            return;
        };
        if let Err(error) = self
            .write_epoch(origin, method, params, epoch.saturating_add(1))
            .await
        {
            tracing::warn!(%error, "MCP 磁盘缓存失效标记写入失败，已停用本次失效");
        }
        drop(lock);
    }

    async fn lock(&self) -> Option<std::fs::File> {
        let path = self.state_path.join("cache.lock");
        // root/state -> root：仅在 Unix 收紧缓存根到 0700，阻断其他 uid 进入，
        // 其下 content/、state/ 即便沿用默认 umask 权限也不可达。其余代码全平台可编译。
        #[cfg(unix)]
        let cache_root = self.state_path.parent().map(PathBuf::from);
        let file = tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(root) = cache_root {
                    std::fs::create_dir_all(&root)?;
                    let mut perms = std::fs::metadata(&root)?.permissions();
                    perms.set_mode(0o700);
                    std::fs::set_permissions(&root, perms)?;
                }
            }
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

    async fn read_epoch(&self, origin: &str, method: &str, params: &str) -> Option<u64> {
        let method_epoch = self.read_epoch_file(origin, method, "*").await?;
        if params.is_empty() || params == "*" {
            return Some(method_epoch);
        }
        let params_epoch = self.read_epoch_file(origin, method, params).await?;
        Some(method_epoch.wrapping_add(params_epoch))
    }

    async fn read_epoch_file(&self, origin: &str, method: &str, params: &str) -> Option<u64> {
        let path = self.epoch_path(origin, method, params);
        match tokio::fs::read(&path).await {
            Ok(bytes) => std::str::from_utf8(&bytes).ok()?.trim().parse().ok(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.write_epoch(origin, method, params, 0).await.ok()?;
                Some(0)
            }
            Err(_) => None,
        }
    }

    async fn write_epoch(
        &self,
        origin: &str,
        method: &str,
        params: &str,
        epoch: u64,
    ) -> Result<(), std::io::Error> {
        let path = self.epoch_path(origin, method, params);
        let parent = path.parent().expect("epoch path has parent");
        tokio::fs::create_dir_all(parent).await?;
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, epoch.to_string()).await?;
        tokio::fs::rename(tmp, path).await
    }

    fn epoch_path(&self, origin: &str, method: &str, params: &str) -> PathBuf {
        let params = if params.is_empty() { "*" } else { params };
        self.state_path
            .join(EPOCH_DIR)
            .join(digest(&format!("{origin}\0{method}\0{params}")))
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

fn status_key(origin: &str, method: &str) -> String {
    let domain = if method.starts_with("skills/") {
        "skills/"
    } else if method.starts_with("tools/") {
        "tools/"
    } else {
        "resources/"
    };
    format!("{origin}\0{domain}")
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
