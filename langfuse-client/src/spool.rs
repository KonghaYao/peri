//! 遥测暂存端口：对象身份、所有权句柄与 crate 内可替换的存储端口（契约 §3.2）。
//!
//! 端口方法一律同步且可能阻塞，只允许在 Batcher 所属、有并发上限的 blocking job 中执行；
//! controller/async worker 不直接调用。本模块只交付端口声明与所有权类型，native 适配器
//! （`spool/native*.rs`）属 P1c，路径只允许出现在适配器/ACP 装配层。

pub(crate) mod sealed {
    /// 封闭实现范围：只有 langfuse-client crate 内可 `impl SpoolStore`。
    /// 外部 crate 只能消费/注入已构造的 capability（契约 §3.2）。
    pub trait Sealed {}
}

pub(crate) mod cleanup;
pub mod error;
pub mod types;

pub use cleanup::CleanupRegistration;
pub use error::SpoolError;
pub use types::{
    ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor, QuotaReservation, RouteFingerprint,
    SpoolObject, SpoolPage, SpoolState, SpoolStore, SpoolUsage, TemporaryObject,
};

use sha2::{Digest, Sha256};

use crate::error::LangfuseError;

/// 端口结果；错误分类见 [`SpoolError`]。
pub type SpoolResult<T> = Result<T, SpoolError>;

/// 页游标长度上限（契约 §3.3：cursor 限 4 KiB）；超限拒绝而不是截断。
pub(crate) const MAX_PAGE_CURSOR_BYTES: usize = 4 * 1024;

/// 同步可读的正文句柄，只能由端口实现返回；从 0 读，仅可读完整发布对象。
pub type SpoolReader = Box<dyn std::io::Read + Send>;

/// 由 OTLP endpoint 与凭据派生的 route 摘要（长度分帧 SHA-256，不记录原值）。
///
/// 规范化 endpoint：拒绝 userinfo/query/fragment，保留合法 path 前缀，scheme/host/默认 port
/// 正规化并去尾斜杠。任一轮换凭据都会换算出新摘要，从而隔离旧对象。
///
/// 长度分帧使相邻字段的边界不可伪造（`len || bytes` 逐段入哈希），只有 http/https
/// 这类带 host 的层次化端点被接受；任何解析/规范化失败都返回 [`LangfuseError::Config`]，
/// 且错误文本不回显 endpoint 或凭据。
pub fn route_fingerprint(
    endpoint: &str,
    public_key: &str,
    secret_key: &str,
) -> Result<RouteFingerprint, LangfuseError> {
    let normalized = normalize_endpoint(endpoint)?;
    let mut hasher = Sha256::new();
    for part in [
        normalized.as_bytes(),
        public_key.as_bytes(),
        secret_key.as_bytes(),
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    Ok(RouteFingerprint::from_bytes(bytes))
}

/// 把 endpoint 正规化为 `scheme://host[:port]/path` 形式（无尾斜杠、无凭据）。
fn normalize_endpoint(endpoint: &str) -> Result<String, LangfuseError> {
    let invalid = || {
        LangfuseError::Config(
            "langfuse endpoint must be a credential-free http(s) URL without query or fragment"
                .into(),
        )
    };
    let mut url = reqwest::Url::parse(endpoint).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid());
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    // `Url` 已把 scheme/host 小写化并去掉默认端口；这里只统一 path 尾斜杠。
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
pub mod native;

#[cfg(test)]
#[path = "spool_test.rs"]
mod tests;
