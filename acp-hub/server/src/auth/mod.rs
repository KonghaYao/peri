//! 认证/授权模块（Feature F2）：token 模型、TokenStore、instance 双向认证
//! （HMAC challenge-response，§9.2）、连接身份上下文（§9.5）。
//!
//! 复用 `acp-hub-proto` 密码原语（`hmac.rs`），不重复实现：nonce/session
//! context 生成、HKDF 密钥派生、MAC 输入规范化、常量时间 MAC 校验。
//!
//! 脱敏纪律（§9.3）：token 本体/nonce/派生密钥/HMAC 输出永不进入日志、
//! 审计、错误 Display（本模块内仅 [`TokenRecord.token`] 持有，落 0600 文件）。

pub mod audit;
mod nonce;
pub use nonce::{NonceRegistry, NonceVerdict};

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use acp_hub_proto::conn::{Auth, AuthResponse};
use acp_hub_proto::hmac::{
    compute_mac, derive_mac_key, generate_connection_context, mac_input, CHALLENGE_NONCE_LEN,
};
use acp_hub_proto::instance::InstanceHello;
use acp_hub_proto::version::PROTOCOL_VERSION;
use acp_hub_proto::whitelist::Role;

use crate::auth::audit::audit;

/// token 存储文件名（`<config_dir>/tokens.toml`，0600，§4.3.2）。
pub const TOKENS_FILE: &str = "tokens.toml";

/// token 文件格式版本（§4.3.1：`version = 1`，未来格式演进可迁移）。
pub const TOKENS_FILE_VERSION: u32 = 1;

/// token 本体长度：32B CSPRNG → base64 标准字母表 44 字符（§9.2.1）。
pub const TOKEN_B64_LEN: usize = 44;

/// 未知 token 的失败计数 key（§4.8：泄露检测依赖按 token_id 可查失败次数，
/// 未知 token 无 id，需与已知 token 的失败区分呈现）。
pub const UNKNOWN_TOKEN_ID: &str = "<unknown>";

/// bootstrap 自动生成 instance token 的名称（§3.3【决策】；§4.5 instance_id =
/// token name）。必须与 `channel::DEFAULT_MACHINE_ID`（"local"）一致：本机
/// bootstrap 机器即「缺省本机」路由目标（§4.3 P5），否则 client 不带
/// instanceId 的 create 会命中 `UnknownInstance("local")`（E3 链路断点）。
pub const BOOTSTRAP_INSTANCE_NAME: &str = "local";
pub const BROWSER_COOKIE: &str = "acp_hub_session";
pub(crate) const BROWSER_SESSION_TTL_SECS: u64 = 8 * 3600;
const BROWSER_SESSION_TTL: std::time::Duration =
    std::time::Duration::from_secs(BROWSER_SESSION_TTL_SECS);
const BROWSER_SESSION_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// TokenRole（token 三级角色，§9.2.2 + §9.5）
// ---------------------------------------------------------------------------

/// token 三级角色。串行化为 kebab-case 字符串（tokens.toml / CLI）。
///
/// 与线级 [`Role`]（`whitelist::Role { Client, Instance }`）的映射唯一入口是
/// [`TokenRole::wire_role`]——read-only 与 full 在线级同属 Client，其写权限
/// 差异由 gateway 用 [`ConnectionCtx::can_send_action`] 在帧级强制（§5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenRole {
    /// 收 spawn/kill 指令、上报事件/心跳（§9.2，双向认证）。
    Instance,
    /// client：读全部 Doc + 发 Action（TUI）。
    Full,
    /// client：仅读 yjs 状态与订阅事件流（M3 Web 面板，M1 预留档位）。
    ReadOnly,
}

impl TokenRole {
    /// 线级连接角色（`whitelist::Role`）：instance→Instance；full/read-only→Client。
    pub fn wire_role(self) -> Role {
        match self {
            TokenRole::Instance => Role::Instance,
            TokenRole::Full | TokenRole::ReadOnly => Role::Client,
        }
    }

    /// 是否可发 Action（instance/full 可；read-only 不可，M1 即强制，§9.2.2）。
    pub fn can_send_action(self) -> bool {
        !matches!(self, TokenRole::ReadOnly)
    }

    /// HMAC 派生 role 字符串（§9.2：仅 instance 走双向认证，取值恒为
    /// `"instance"`）；CLI/toml 的 kebab-case 形态。
    pub fn as_str(self) -> &'static str {
        match self {
            TokenRole::Instance => "instance",
            TokenRole::Full => "full",
            TokenRole::ReadOnly => "read-only",
        }
    }
}

impl std::fmt::Display for TokenRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TokenRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "instance" => Ok(TokenRole::Instance),
            "full" => Ok(TokenRole::Full),
            "read-only" => Ok(TokenRole::ReadOnly),
            other => Err(format!(
                "非法角色 {other:?}（可选 instance/full/read-only）"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// TokenRecord / TokenInfo（§4.2）
// ---------------------------------------------------------------------------

/// 存储态记录（含 token 本体，仅内存与 0600 文件内存在）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenRecord {
    /// uuid v4（视图/审计/吊销引用键）。
    pub id: String,
    /// token 角色。
    pub role: TokenRole,
    /// instance：hostname；client：运维命名（如「桌面 TUI」）。
    pub name: String,
    /// 32B CSPRNG，base64（44 字符，§9.2.1「32B CSPRNG」）。
    pub token: String,
    /// RFC3339。
    pub created_at: DateTime<Utc>,
    /// 吊销态（吊销即刻生效，§9.2.1）。
    pub revoked: bool,
}

/// 对外视图（§9.2.1：只暴露 token_id，**绝不暴露 token 本体**——结构级
/// 保证，编译期不可外泄）。
#[derive(Debug, Clone, PartialEq)]
pub struct TokenInfo {
    /// uuid v4。
    pub id: String,
    /// token 角色。
    pub role: TokenRole,
    /// 展示名。
    pub name: String,
    /// 签发时间（RFC3339）。
    pub created_at: DateTime<Utc>,
    /// 吊销态。
    pub revoked: bool,
}

impl From<&TokenRecord> for TokenInfo {
    fn from(r: &TokenRecord) -> Self {
        TokenInfo {
            id: r.id.clone(),
            role: r.role,
            name: r.name.clone(),
            created_at: r.created_at,
            revoked: r.revoked,
        }
    }
}

impl std::fmt::Display for TokenInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:<36}  {:<10}  {:<20}  {}  {}",
            self.id,
            self.role,
            self.name,
            self.created_at.to_rfc3339(),
            if self.revoked { "revoked" } else { "active" }
        )
    }
}

// ---------------------------------------------------------------------------
// 错误面（§4.5）
// ---------------------------------------------------------------------------

/// store 错误（token 文件面）。
#[derive(Debug, Error)]
pub enum StoreError {
    /// 文件存在但不可解析/缺字段（拒绝启动，不静默覆盖，§4.3）。
    #[error("token 文件格式非法: {0}")]
    Format(String),
    /// 文件版本不匹配（未来格式演进）。
    #[error("token 文件版本不支持: {0}")]
    Version(u32),
    /// token 长度非法（需 44 字符 base64，§4.3.3 长度防御）。
    #[error("token 长度非法（需 {TOKEN_B64_LEN} 字符 base64）")]
    BadTokenLength,
    /// 文件 I/O 失败。
    #[error("token 文件 I/O 失败: {0}")]
    Io(#[from] std::io::Error),
    /// 序列化失败。
    #[error("token 文件序列化失败: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// 反序列化失败。
    #[error("token 文件反序列化失败: {0}")]
    Deserialize(#[from] toml::de::Error),
    /// 生成碰撞（重试后仍重复）。
    #[error("token 生成碰撞（重试后仍重复）")]
    Collision,
    /// 持久化失败（fsync/rename 等）。
    #[error("token 持久化失败: {0}")]
    Persist(String),
}

/// 认证错误面。
///
/// **脱敏**：变体不携带 token 本体（token_id 非凭证材料，§9.2.1 视图对象即
/// 暴露 token_id；携带它仅为审计计数，§4.8）。Display 不含任何凭证材料。
///
/// **关闭码映射**（§4.5 失败语义）：任何失败 → 关闭连接，instance 用
/// `CLOSE_CONFIG_FATAL`(4502)；client 认证失败同用 4502（token 错误属配置性
/// 永久失败，重连无益）。实际关闭由 F5 gateway 执行（本模块只产出错误）。
#[derive(Debug, Error)]
pub enum AuthError {
    /// nonce 非 base64 / 非 32B。
    #[error("nonce 编码非法")]
    BadNonceEncoding,
    /// nonce 重放（窗口内重复提交）。
    #[error("nonce 重放")]
    ReplayNonce,
    /// nonce 过期（当前流程按新 nonce 处理，见 [`NonceVerdict::Expired`]）。
    #[error("nonce 过期")]
    ExpiredNonce,
    /// token 未登记。
    #[error("token 未登记")]
    UnknownToken,
    /// token 已吊销（携带 id 供按 token_id 计数，§4.8）。
    #[error("token 已吊销")]
    RevokedToken {
        /// 吊销记录 id。
        token_id: String,
    },
    /// 角色不匹配（如 instance/hello 提交 client token）。
    #[error("角色不匹配")]
    RoleMismatch {
        /// 匹配记录 id。
        token_id: String,
    },
    /// store 错误。
    #[error("store 错误: {0}")]
    Store(#[from] StoreError),
}

impl AuthError {
    /// 失败计数/审计用的稳定结果串（脱敏、可聚合，§9.4）。
    fn result_key(&self) -> &'static str {
        match self {
            AuthError::BadNonceEncoding => "bad_nonce",
            AuthError::ReplayNonce => "replay_nonce",
            AuthError::ExpiredNonce => "expired_nonce",
            AuthError::UnknownToken => "unknown_token",
            AuthError::RevokedToken { .. } => "revoked_token",
            AuthError::RoleMismatch { .. } => "role_mismatch",
            AuthError::Store(_) => "store_error",
        }
    }

    /// 已知记录 id（RevokedToken/RoleMismatch 时）；其余为 None → 计数
    /// key 取 [`UNKNOWN_TOKEN_ID`]。
    fn token_id(&self) -> Option<&str> {
        match self {
            AuthError::RevokedToken { token_id } => Some(token_id),
            AuthError::RoleMismatch { token_id } => Some(token_id),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// TokenStore（§4.3）
// ---------------------------------------------------------------------------

/// token 存储：加载/生成/吊销/校验 + 原子写（§4.3.2）+ mtime 惰性重载
/// （§4.3.3）。
///
/// 并发模型（§4.3.2【决策】）：单 server 进程持有内存 store；CLI
/// `token generate/revoke` 直写同一文件。不做文件锁——mtime 惰性重载消除
/// 「CLI 改完 server 不认」的主分歧；两写互相覆盖的竞态窗口接受为已知限制。
pub struct TokenStore {
    path: PathBuf,
    records: Vec<TokenRecord>,
    last_mtime: Option<SystemTime>,
}

/// tokens.toml 文件形态（§4.3.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokensFile {
    #[serde(default = "default_file_version")]
    version: u32,
    #[serde(default)]
    tokens: Vec<TokenRecord>,
}

fn default_file_version() -> u32 {
    TOKENS_FILE_VERSION
}

impl TokenStore {
    /// 加载：文件不存在 → 空 store；存在且坏格式/坏 token 长度 → Err
    /// （拒绝启动，不静默覆盖，§4.3）。
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        let mut store = TokenStore {
            path: path.to_path_buf(),
            records: Vec::new(),
            last_mtime: None,
        };
        store.reload()?;
        Ok(store)
    }

    /// 生成并持久化（原子写，§4.3.2）。
    ///
    /// token：32B CSPRNG → base64（44 字符）；生成时防碰撞重试（hashset 查重）。
    pub fn generate(&mut self, role: TokenRole, name: &str) -> Result<TokenRecord, StoreError> {
        self.maybe_reload();
        let existing: HashSet<String> = self.records.iter().map(|r| r.token.clone()).collect();
        let token = (0..3)
            .find_map(|_| {
                let t = generate_token();
                (!existing.contains(&t)).then_some(t)
            })
            .ok_or(StoreError::Collision)?;
        let record = TokenRecord {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            name: name.to_string(),
            token,
            created_at: Utc::now(),
            revoked: false,
        };
        self.records.push(record.clone());
        if let Err(e) = self.persist() {
            // 回滚内存态，保持与磁盘一致（T4：写失败 → Err 且原文件完好）。
            self.records.pop();
            return Err(e);
        }
        Ok(record)
    }

    /// 吊销并持久化（幂等：已吊销/不存在返回 `Ok(None)`，不报错）。
    ///
    /// 吊销即刻生效（§9.2.1 密钥生命周期）；运行中 server 经 mtime 重载
    /// 在下次 `validate` 时生效。
    pub fn revoke(&mut self, id: &str) -> Result<Option<TokenRecord>, StoreError> {
        self.maybe_reload();
        let Some(rec) = self.records.iter_mut().find(|r| r.id == id) else {
            return Ok(None);
        };
        if rec.revoked {
            return Ok(None);
        }
        rec.revoked = true;
        let rec = rec.clone();
        self.persist()?;
        Ok(Some(rec))
    }

    /// 视图列表（无 token 本体，§9.2.1）。
    pub fn list(&self) -> Vec<TokenInfo> {
        self.records.iter().map(TokenInfo::from).collect()
    }

    /// 校验：常量时间比较 + 角色精确匹配 + 吊销态；**每次调用先按 mtime
    /// 惰性重载**（§4.3.3）。宽限期轮换：新旧 token 并存期间同时有效。
    pub fn validate(
        &mut self,
        candidate: &str,
        required: TokenRole,
    ) -> Result<TokenRecord, AuthError> {
        self.maybe_reload();
        for r in &self.records {
            if r.revoked {
                continue;
            }
            if constant_time_eq(candidate, &r.token) {
                if r.role != required {
                    return Err(AuthError::RoleMismatch {
                        token_id: r.id.clone(),
                    });
                }
                return Ok(r.clone());
            }
        }
        // 区分「已吊销」与「未登记」（H9：分开计数）。
        for r in &self.records {
            if r.revoked && constant_time_eq(candidate, &r.token) {
                return Err(AuthError::RevokedToken {
                    token_id: r.id.clone(),
                });
            }
        }
        Err(AuthError::UnknownToken)
    }

    /// client 认证校验（§4.6）：角色允许集 = Full | ReadOnly；Instance 角色
    /// token 提交 client 认证 → [`AuthError::RoleMismatch`]（防 token 跨面复用）。
    pub fn validate_client(&mut self, candidate: &str) -> Result<TokenRecord, AuthError> {
        self.maybe_reload();
        for r in &self.records {
            if r.revoked {
                continue;
            }
            if constant_time_eq(candidate, &r.token) {
                if r.role == TokenRole::Instance {
                    return Err(AuthError::RoleMismatch {
                        token_id: r.id.clone(),
                    });
                }
                return Ok(r.clone());
            }
        }
        for r in &self.records {
            if r.revoked && constant_time_eq(candidate, &r.token) {
                return Err(AuthError::RevokedToken {
                    token_id: r.id.clone(),
                });
            }
        }
        Err(AuthError::UnknownToken)
    }

    pub fn validate_client_id(&mut self, id: &str) -> Result<TokenRecord, AuthError> {
        self.maybe_reload();
        match self.records.iter().find(|r| r.id == id) {
            Some(r) if r.revoked => Err(AuthError::RevokedToken {
                token_id: id.to_string(),
            }),
            Some(r) if r.role == TokenRole::Instance => Err(AuthError::RoleMismatch {
                token_id: id.to_string(),
            }),
            Some(r) => Ok(r.clone()),
            None => Err(AuthError::UnknownToken),
        }
    }

    /// §3.3/§4.3.4 启动 bootstrap：不存在任何未吊销 instance 角色 token 时
    /// 自动生成一个（name = `"bootstrap-instance"`）。返回是否生成了新 token
    /// （打印与审计由装配方 main.rs 执行——token 本体只进终端一次，不进日志）。
    pub fn ensure_instance_token(&mut self) -> Result<Option<TokenRecord>, StoreError> {
        self.maybe_reload();
        if self
            .records
            .iter()
            .any(|r| r.role == TokenRole::Instance && !r.revoked)
        {
            return Ok(None);
        }
        let rec = self.generate(TokenRole::Instance, BOOTSTRAP_INSTANCE_NAME)?;
        Ok(Some(rec))
    }

    /// 已登记记录数（供 bootstrap/诊断）。
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// 惰性重载：`stat` 文件 mtime，与 `last_mtime` 不一致则重载并合并——
    /// CLI 的 generate/revoke 对运行中 server **即时生效**（M1 无控制面，
    /// 这是最小实现，§4.3.3【决策】）。重载失败（文件被手改坏）→ 保持旧
    /// 内存态 + `error!` 审计（不静默，也不因手改挂掉服务）。
    fn maybe_reload(&mut self) {
        let mtime = current_mtime(&self.path);
        if mtime == self.last_mtime {
            return;
        }
        match load_records(&self.path) {
            Ok(records) => {
                self.records = records;
                self.last_mtime = mtime;
            }
            Err(e) => {
                tracing::error!(
                    target: "acp_hub.audit",
                    action = "auth.store_reload",
                    result = "failed",
                    reason = e.to_string(),
                );
            }
        }
    }

    /// 全量重载（load 入口；文件坏格式 → Err，拒绝启动）。
    fn reload(&mut self) -> Result<(), StoreError> {
        self.records = load_records(&self.path)?;
        self.last_mtime = current_mtime(&self.path);
        Ok(())
    }

    /// 原子写（§4.3.2）：序列化 → 同目录 `.tmp` 文件（0600）→ fsync →
    /// rename 覆盖 → 目录 fsync。崩溃不产生半文件；token 是安全关键资产，
    /// fsync 不可省（server 重启后丢失新 token = 机器被锁）。
    fn persist(&mut self) -> Result<(), StoreError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| StoreError::Persist("token 路径无父目录".to_string()))?;
        if !dir.exists() {
            return Err(StoreError::Persist(format!(
                "token 目录不存在: {}",
                dir.display()
            )));
        }
        let file = TokensFile {
            version: TOKENS_FILE_VERSION,
            tokens: self.records.clone(),
        };
        let content = toml::to_string(&file)?;
        let tmp = dir.join(format!(".tokens.toml.tmp.{}", std::process::id()));
        if let Err(e) = write_atomic_tmp(&tmp, &content) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(StoreError::Io(e));
        }
        // 目录 fsync（§4.3.2）。
        if let Ok(d) = fs::File::open(dir) {
            d.sync_all()
                .map_err(|e| StoreError::Persist(format!("目录 fsync 失败: {e}")))?;
        }
        self.last_mtime = current_mtime(&self.path);
        Ok(())
    }
}

/// 写 `.tmp` 文件：create（0600）→ 写入 → fsync。
fn write_atomic_tmp(tmp: &Path, content: &str) -> Result<(), StoreError> {
    let mut f = fs::File::create(tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(content.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// 读取并校验 records（不触碰 mtime）。
fn load_records(path: &Path) -> Result<Vec<TokenRecord>, StoreError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    let file: TokensFile = toml::from_str(&content)?;
    if file.version != TOKENS_FILE_VERSION {
        return Err(StoreError::Version(file.version));
    }
    for r in &file.tokens {
        if !is_valid_token(&r.token) {
            return Err(StoreError::BadTokenLength);
        }
    }
    Ok(file.tokens)
}

fn current_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// token 本体合法性：44 字符 base64 且解码为 32B（§4.3.3 长度防御）。
fn is_valid_token(s: &str) -> bool {
    if s.len() != TOKEN_B64_LEN {
        return false;
    }
    matches!(
        base64::engine::general_purpose::STANDARD.decode(s),
        Ok(b) if b.len() == 32
    )
}

/// 32B CSPRNG → base64（44 字符，§9.2.1）。
fn generate_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// 常量时间比较（§4.3.3【决策】；`subtle` 未预填，退化为手写 XOR-fold）。
///
/// token 为固定 44 字符（加载时断言）；长度分支只泄露「候选是否 44 字符」，
/// 无凭证价值（长度防御语义）。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

// ---------------------------------------------------------------------------
// 认证失败计数（§17.1 / §4.8）
// ---------------------------------------------------------------------------

/// 认证失败计数：按 token_id（内存 `Mutex<HashMap>`）+ 全局 `AtomicU64`。
///
/// 审计事件携带 `auth_failed_total` 快照字段，结构化日志即聚合事实源
/// （§17.1：M1 不建独立指标系统）。未知 token 的 key 取 [`UNKNOWN_TOKEN_ID`]。
#[derive(Debug, Default)]
pub struct AuthStats {
    by_token: Mutex<HashMap<String, u64>>,
    total: AtomicU64,
}

impl AuthStats {
    /// 认证失败计数递增。
    pub fn record_failure(&self, token_id: &str) {
        *self
            .by_token
            .lock()
            .expect("AuthStats mutex poisoned")
            .entry(token_id.to_string())
            .or_insert(0) += 1;
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// 某 token_id（或 `"<unknown>"`）的失败次数。
    pub fn failures_for(&self, token_id: &str) -> u64 {
        self.by_token
            .lock()
            .expect("AuthStats mutex poisoned")
            .get(token_id)
            .copied()
            .unwrap_or(0)
    }

    /// 全局失败次数。
    pub fn total_failures(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// ConnectionCtx / InstanceAuthOk / AuthService（§4.5–§4.7）
// ---------------------------------------------------------------------------

/// 认证通过后的连接身份上下文（gateway F5 持有，贯穿连接生命周期）。
#[derive(Debug, Clone)]
pub struct ConnectionCtx {
    /// 身份：token_id（审计/吊销引用键）。
    pub token_id: String,
    /// 身份级角色（§9.5：token 即身份）。
    pub role: TokenRole,
    /// TokenRecord.name。
    pub name: String,
    /// 绑定信息：远端地址（非回环拒绝判定输入，§3.7）。
    pub peer: SocketAddr,
    /// 绑定信息：instance 专属（hello.hostname）。
    pub hostname: Option<String>,
    /// 建立时间。
    pub established_at: DateTime<Utc>,
}

impl ConnectionCtx {
    /// 线级连接角色（§4.1 映射，`whitelist::Role`）。
    pub fn wire_role(&self) -> Role {
        self.role.wire_role()
    }

    /// 是否可发 Action（instance/full 可；read-only 不可，M1 即强制，§9.2.2）。
    pub fn can_send_action(&self) -> bool {
        self.role.can_send_action()
    }
}

/// instance 认证成功产物：连接上下文 + 待 gateway 下发的 auth_response。
#[derive(Debug, Clone)]
pub struct InstanceAuthOk {
    /// 连接身份上下文。
    pub ctx: ConnectionCtx,
    /// `auth_response` 载荷（`{ connection_context: b64, hmac: b64 }`，§9.2 步骤 2）。
    pub response: AuthResponse,
}

/// 认证服务：instance 双向认证（§9.2）+ client 单向认证（§4.6）。
///
/// 服务端时序（§4.5）：nonce 解码 → 防重放（**先于 token 校验**，认证失败
/// 的 nonce 同样登记，防「失败后重放成功路径」）→ token 校验 → 生成
/// connection_context → HKDF 派生密钥 → 计算 MAC → 下发 auth_response。
/// 权威时钟在 server（[`Instant`]）。
pub struct AuthService {
    store: TokenStore,
    nonces: NonceRegistry,
    stats: AuthStats,
    browser_sessions: HashMap<String, BrowserSession>,
}

#[derive(Clone)]
struct BrowserSession {
    token_id: String,
    expires_at: Instant,
}

impl AuthService {
    /// 以已加载的 [`TokenStore`] 构建（nonce 注册表与失败计数初始为空）。
    pub fn new(store: TokenStore) -> Self {
        AuthService {
            store,
            nonces: NonceRegistry::new(),
            stats: AuthStats::default(),
            browser_sessions: HashMap::new(),
        }
    }

    pub fn create_browser_session(
        &mut self,
        bearer: &str,
    ) -> Result<(String, ConnectionCtx), AuthError> {
        self.sweep_browser_sessions();
        let record = self.store.validate_client(bearer)?;
        if self.browser_sessions.len() >= BROWSER_SESSION_CAPACITY {
            if let Some(oldest) = self
                .browser_sessions
                .iter()
                .min_by_key(|(_, s)| s.expires_at)
                .map(|(id, _)| id.clone())
            {
                self.browser_sessions.remove(&oldest);
            }
        }
        let mut raw = [0u8; 32];
        rand::rng().fill_bytes(&mut raw);
        let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        self.browser_sessions.insert(
            id.clone(),
            BrowserSession {
                token_id: record.id.clone(),
                expires_at: Instant::now() + BROWSER_SESSION_TTL,
            },
        );
        Ok((id, browser_ctx(record, "127.0.0.1:0".parse().unwrap())))
    }

    pub fn validate_browser_session(
        &mut self,
        id: &str,
        peer: SocketAddr,
    ) -> Result<ConnectionCtx, AuthError> {
        self.sweep_browser_sessions();
        let token_id = self
            .browser_sessions
            .get(id)
            .ok_or(AuthError::UnknownToken)?
            .token_id
            .clone();
        let record = self.store.validate_client_id(&token_id)?;
        Ok(browser_ctx(record, peer))
    }

    pub fn delete_browser_session(&mut self, id: &str) -> bool {
        self.browser_sessions.remove(id).is_some()
    }
    pub fn revalidate_client_identity(
        &mut self,
        token_id: &str,
        expected_role: TokenRole,
    ) -> Result<(), AuthError> {
        let record = self.store.validate_client_id(token_id)?;
        if record.role != expected_role {
            return Err(AuthError::RoleMismatch {
                token_id: token_id.to_string(),
            });
        }
        Ok(())
    }
    pub fn revalidate_instance_identity(&mut self, token_id: &str) -> Result<(), AuthError> {
        self.store.maybe_reload();
        match self.store.records.iter().find(|r| r.id == token_id) {
            Some(r) if !r.revoked && r.role == TokenRole::Instance => Ok(()),
            Some(r) if r.revoked => Err(AuthError::RevokedToken {
                token_id: token_id.to_string(),
            }),
            Some(_) => Err(AuthError::RoleMismatch {
                token_id: token_id.to_string(),
            }),
            None => Err(AuthError::UnknownToken),
        }
    }
    fn sweep_browser_sessions(&mut self) {
        let now = Instant::now();
        self.browser_sessions.retain(|_, s| s.expires_at > now);
    }

    /// 失败计数视图（审计/指标聚合）。
    pub fn stats(&self) -> &AuthStats {
        &self.stats
    }

    /// 底层 store（bootstrap/运维操作）。
    pub fn store_mut(&mut self) -> &mut TokenStore {
        &mut self.store
    }

    /// nonce 注册表（F5 gateway 周期任务与心跳同 tick 调 `sweep`，§4.4）。
    pub fn nonces_mut(&mut self) -> &mut NonceRegistry {
        &mut self.nonces
    }

    /// instance 连接认证（§9.2 步骤 1–2，服务端身份证明）。
    ///
    /// 签名保持 async 与设计文档一致（F5 装配后可引入 IO）；当前实现无
    /// await 点。
    #[allow(clippy::unused_async)]
    pub async fn authenticate_instance(
        &mut self,
        hello: &InstanceHello,
        peer: SocketAddr,
    ) -> Result<InstanceAuthOk, AuthError> {
        let start = Instant::now();
        // 1. nonce: base64 解码 → [u8; 32]（失败 → BadNonceEncoding）。
        let nonce = match decode_nonce(&hello.nonce) {
            Ok(n) => n,
            Err(e) => return Err(self.fail_auth("auth.instance", None, e, start)),
        };
        // 2. 防重放（token 校验之前，§4.4）。
        match self.nonces.check_and_mark(&nonce, Instant::now()) {
            NonceVerdict::Accepted => {}
            NonceVerdict::Replay => {
                return Err(self.fail_auth("auth.instance", None, AuthError::ReplayNonce, start))
            }
            NonceVerdict::Expired => {
                return Err(self.fail_auth("auth.instance", None, AuthError::ExpiredNonce, start))
            }
        }
        // 3. token 校验（角色必须为 Instance）。
        let record = match self.store.validate(&hello.token, TokenRole::Instance) {
            Ok(r) => r,
            Err(e) => {
                let tid = e.token_id().map(ToOwned::to_owned);
                return Err(self.fail_auth("auth.instance", tid.as_deref(), e, start));
            }
        };
        // 4–7. connection_context + 派生密钥 + MAC（复用 proto 原语）。
        let token_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(&record.token)
            .expect("token 加载时已断言 44 字符 base64")
            .try_into()
            .expect("44 字符 base64 解码为 32B");
        let context = generate_connection_context();
        let key = derive_mac_key(&token_bytes, TokenRole::Instance.as_str());
        let input = mac_input(
            &nonce,
            &context,
            &PROTOCOL_VERSION.to_string(),
            TokenRole::Instance.as_str(),
        );
        let hmac = compute_mac(&key, &input);
        let ctx = ConnectionCtx {
            token_id: record.id.clone(),
            role: record.role,
            name: record.name.clone(),
            peer,
            hostname: Some(hello.hostname.clone()),
            established_at: Utc::now(),
        };
        audit(
            "auth.instance",
            None,
            Some(&record.id),
            "ok",
            start.elapsed(),
            None,
        );
        Ok(InstanceAuthOk {
            ctx,
            response: AuthResponse {
                connection_context: base64::engine::general_purpose::STANDARD.encode(context),
                hmac: base64::engine::general_purpose::STANDARD.encode(hmac),
            },
        })
    }

    /// client 连接认证（单向，`auth` 帧，§4.6）。
    ///
    /// client 无 HMAC（§9.2 明示仅覆盖 instance 连接）；read-only 的帧级写
    /// 限制由 gateway 用 [`ConnectionCtx::can_send_action`] 强制（§5）。
    #[allow(clippy::unused_async)]
    pub async fn authenticate_client(
        &mut self,
        auth: &Auth,
        peer: SocketAddr,
    ) -> Result<ConnectionCtx, AuthError> {
        let start = Instant::now();
        let record = match self.store.validate_client(&auth.token) {
            Ok(r) => r,
            Err(e) => {
                let tid = e.token_id().map(ToOwned::to_owned);
                return Err(self.fail_auth("auth.client", tid.as_deref(), e, start));
            }
        };
        let ctx = ConnectionCtx {
            token_id: record.id.clone(),
            role: record.role,
            name: record.name.clone(),
            peer,
            hostname: None,
            established_at: Utc::now(),
        };
        audit(
            "auth.client",
            None,
            Some(&record.id),
            "ok",
            start.elapsed(),
            None,
        );
        Ok(ctx)
    }

    /// 失败路径统一处理：计数（未知 token → [`UNKNOWN_TOKEN_ID`]）+ 审计
    /// `auth.* failed`（携带 `auth_failed_total` 快照，§4.8）+ 原样返回错误
    /// （不静默，§9.2 失败语义）。
    fn fail_auth(
        &self,
        action: &str,
        token_id: Option<&str>,
        err: AuthError,
        start: Instant,
    ) -> AuthError {
        let key = token_id.unwrap_or(UNKNOWN_TOKEN_ID);
        self.stats.record_failure(key);
        audit(
            action,
            None,
            Some(key),
            err.result_key(),
            start.elapsed(),
            Some(self.stats.total_failures()),
        );
        err
    }
}

fn browser_ctx(record: TokenRecord, peer: SocketAddr) -> ConnectionCtx {
    ConnectionCtx {
        token_id: record.id,
        role: record.role,
        name: record.name,
        peer,
        hostname: None,
        established_at: Utc::now(),
    }
}

/// nonce 解码：base64 → [u8; 32]（失败 → [`AuthError::BadNonceEncoding`]）。
fn decode_nonce(s: &str) -> Result<[u8; CHALLENGE_NONCE_LEN], AuthError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| AuthError::BadNonceEncoding)?;
    bytes.try_into().map_err(|_| AuthError::BadNonceEncoding)
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod auth_test;
