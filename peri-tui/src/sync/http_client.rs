//! 新协议 HTTP 客户端（r2-encrypted-transfer v1）。
//!
//! - 仅 HTTPS（`http://`/`ws://`/`wss://` 一律拒绝；`allow_insecure` 仅测试 override）；
//! - 签名头：`Authorization: PeriSig <device_id> <unix_ts> <signature>`（±300s 由服务端判定）；
//! - 429 解析 Worker 自算的 `Retry-After`；无 header 时按 [`ExponentialBackoff`] 退避；
//! - 所有错误与日志**不含**同步码、channel ID 全文、Authorization、data key 与 payload
//!   （`ApiError.detail` 只含类别级描述，reqwest 错误不携带 URL）。
//!
//! 端点与签名字段序冻结于 03-plan Slice 2（TS Worker 已按同一契约通过测试）：
//! create `[channel_id, sender_device_id, expected_receiver_device_id, sender_ed_pub,
//! sender_x_pub]`；code `[channel_id, epoch, sha256(code_norm)]`；join `[channel_id,
//! code_norm, receiver_device_id, receiver_ed_pub, receiver_x_pub]`；msg（handshake）
//! `[channel_id, seq, sha256(payload)]`；upload `[channel_id, part_index,
//! sha256(ciphertext)]`；download `[channel_id, part_index]`；confirm/revoke
//! `[channel_id]`。

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::sync::canonical;
use crate::sync::device::{self, DeviceId};
use crate::sync::keystore::SecretStore;
use crate::sync::limits;

/// 签名方案标签（进入 `Authorization` 头）。
pub const PERISIG_SCHEME: &str = "PeriSig";

/// 429 客户端重试次数上限（每次按 `Retry-After`/指数退避等待）。
pub const MAX_429_RETRIES: u32 = 3;

/// 校验 server URL：仅 `https://` 合法；`http://`/`ws://`/`wss://` 拒绝。
///
/// `allow_insecure` 仅测试 override，且**只放行 `http`**（L1 复审修复：
/// ws/wss 即使测试 override 也一律拒绝，避免误连旧 WebSocket relay）；
/// 生产路径必须传 `false`。错误消息不含完整 URL 中的路径。
pub fn validate_server_url(url: &str, allow_insecure: bool) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("invalid server URL")?;
    let scheme = parsed.scheme();
    if scheme == "https" {
        return Ok(());
    }
    if allow_insecure && scheme == "http" {
        return Ok(());
    }
    anyhow::bail!(
        "server URL must use https (got scheme '{scheme}'); http is only a test override, \
         ws/wss are always rejected"
    );
}

/// 构造 `Authorization: PeriSig <device_id> <unix_ts> <signature>` 头。
///
/// 签名 bytes 为 `peri-sync/v1|op|field...|unix_seconds`（canonical transcript），
/// 字段顺序即契约；`unix_secs` 由调用方提供（一般取当前墙钟，服务端接受 ±300s）。
pub fn peri_sig_header(
    store: &dyn SecretStore,
    device_id: &DeviceId,
    op: &str,
    fields: &[&str],
    unix_secs: u64,
) -> Result<String> {
    let sig = device::sign_transcript(store, op, fields, unix_secs)?;
    Ok(format!(
        "{} {} {} {}",
        PERISIG_SCHEME,
        device_id.to_b64(),
        unix_secs,
        canonical::b64url_nopad(&sig.to_bytes())
    ))
}

/// 错误类别（客户端侧映射，不含任何敏感值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// 400：请求格式非法。
    BadRequest,
    /// 401：签名无效/过期。
    InvalidSignature,
    /// 403：角色/状态/身份不符。
    Forbidden,
    /// 404：未知、撤销、过期或已清理的 channel / part。
    NotFound,
    /// 409：同 ID 冲突（create）或同 seq/part 异内容。
    Conflict,
    /// 409 COLLISION：40-bit 撞码（可重试一次新码）。
    Collision,
    /// 413：超过 part/预算限额。
    TooLarge,
    /// 429：限流（携带 `Retry-After`）。
    RateLimited,
    /// 5xx：服务端错误。
    ServerError,
    /// 网络/传输层错误（detail 不含 URL）。
    Transport,
    /// 响应体无法解析或未知状态。
    Malformed,
}

/// API 错误。`detail` 只含类别级描述，绝不含同步码/密钥/Authorization/payload/URL。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    /// Worker 返回的 `Retry-After`（秒）。
    pub retry_after_secs: Option<u64>,
    detail: String,
}

impl ApiError {
    pub fn new(kind: ApiErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            retry_after_secs: None,
            detail: detail.into(),
        }
    }

    pub fn with_retry_after(
        kind: ApiErrorKind,
        detail: impl Into<String>,
        retry_after: Option<u64>,
    ) -> Self {
        Self {
            kind,
            retry_after_secs: retry_after,
            detail: detail.into(),
        }
    }

    fn transport_detail(e: &reqwest::Error) -> String {
        if e.is_timeout() {
            "request timed out".to_string()
        } else if e.is_connect() {
            "connection failed".to_string()
        } else if e.is_body() {
            "request body error".to_string()
        } else {
            "network error".to_string()
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            ApiErrorKind::BadRequest => "bad request",
            ApiErrorKind::InvalidSignature => "invalid signature",
            ApiErrorKind::Forbidden => "forbidden",
            ApiErrorKind::NotFound => "not found",
            ApiErrorKind::Conflict => "conflict",
            ApiErrorKind::Collision => "code collision",
            ApiErrorKind::TooLarge => "too large",
            ApiErrorKind::RateLimited => "rate limited",
            ApiErrorKind::ServerError => "server error",
            ApiErrorKind::Transport => "network error",
            ApiErrorKind::Malformed => "malformed response",
        };
        write!(f, "{kind}: {}", self.detail)
    }
}

impl std::error::Error for ApiError {}

/// 429 退避策略（可注入测试）。
pub trait Backoff: Send + Sync {
    /// 第 `attempt` 次重试前的等待时长（attempt 从 1 起）。
    fn delay(&self, attempt: u32, retry_after_secs: Option<u64>) -> Duration;
}

/// 指数退避：有 `Retry-After` 用之（封顶 [`limits::MAX_BACKOFF_MS`]），
/// 否则 `BASE_BACKOFF_MS × 2^(attempt-1)` 封顶 [`limits::MAX_BACKOFF_MS`]。
#[derive(Debug, Default)]
pub struct ExponentialBackoff;

impl Backoff for ExponentialBackoff {
    fn delay(&self, attempt: u32, retry_after_secs: Option<u64>) -> Duration {
        if let Some(secs) = retry_after_secs {
            let capped = secs.min(limits::MAX_BACKOFF_MS / 1000);
            return Duration::from_secs(capped.max(1));
        }
        let shift = attempt.saturating_sub(1).min(10);
        let ms = limits::BASE_BACKOFF_MS
            .saturating_mul(1u64 << shift)
            .min(limits::MAX_BACKOFF_MS);
        Duration::from_millis(ms.max(1))
    }
}

// ─── 公开 DTO（与 TS v1-types.ts 逐字段一致）───────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CreateChannelBody {
    pub channel_id: String,
    pub device_id: String,
    pub expected_device_id: String,
    pub expected_ed_pub: String,
    pub sender_ed_pub: String,
    pub sender_x_pub: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChannelResponse {
    pub channel_id: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterCodeBody {
    /// sender 生成的 8 字符显示格式码（服务端归一化）。
    pub code: String,
    /// 客户端 epoch（unix_secs / 30），仅入签名/审计。
    pub epoch: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpiresAtResponse {
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinBody {
    /// 用户输入的显示格式码（服务端归一化）。
    pub code: String,
    pub device_id: String,
    pub ed_pub: String,
    pub x_pub: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateResponse {
    pub state: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HandshakeBody {
    /// opaque Noise blob（base64url-no-pad）；`None` = 仅拉取对端消息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandshakeResponse {
    pub peer_msg: Option<String>,
    pub state: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadPartBody {
    pub part_index: u64,
    /// AEAD envelope 密文（base64url-no-pad），≤ 64 KiB。
    pub ciphertext: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadPartResponse {
    pub part_index: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LookupResponse {
    pub channel_id: String,
    pub valid_until: u64,
}

/// 端点抽象（channel_flow 依赖此 trait；测试注入 mock 实现）。
#[async_trait]
pub trait ApiClient: Send + Sync {
    async fn create_channel(
        &self,
        body: CreateChannelBody,
        auth: &str,
    ) -> Result<CreateChannelResponse, ApiError>;
    async fn register_code(
        &self,
        channel_id: &str,
        body: RegisterCodeBody,
        auth: &str,
    ) -> Result<ExpiresAtResponse, ApiError>;
    async fn lookup(&self, code_norm: &str) -> Result<LookupResponse, ApiError>;
    async fn join(
        &self,
        channel_id: &str,
        body: JoinBody,
        auth: &str,
    ) -> Result<StateResponse, ApiError>;
    async fn handshake(
        &self,
        channel_id: &str,
        role: &str,
        body: HandshakeBody,
        auth: &str,
    ) -> Result<HandshakeResponse, ApiError>;
    async fn upload_part(
        &self,
        channel_id: &str,
        body: UploadPartBody,
        auth: &str,
    ) -> Result<UploadPartResponse, ApiError>;
    async fn download_part(
        &self,
        channel_id: &str,
        part_index: u64,
        auth: &str,
    ) -> Result<Vec<u8>, ApiError>;
    async fn confirm(&self, channel_id: &str, auth: &str) -> Result<(), ApiError>;
    async fn revoke(&self, channel_id: &str, auth: &str) -> Result<(), ApiError>;
}

/// reqwest 实现（仅 HTTPS；URL 校验在构造时完成）。
pub struct ReqwestClient {
    http: reqwest::Client,
    /// `https://host`（不含路径）。
    base: String,
}

impl ReqwestClient {
    /// 构造客户端；非 HTTPS 的 base URL 立即拒绝（无测试 override）。
    pub fn new(base_url: &str) -> Result<Self> {
        validate_server_url(base_url, false)?;
        let base = base_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to build http client")?;
        Ok(Self { http, base })
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        auth: Option<&str>,
        body_json: Option<&str>,
    ) -> Result<reqwest::Response, ApiError> {
        let url = format!("{}{}", self.base, path);
        let mut req = self
            .http
            .request(method, url)
            .header("content-type", "application/json");
        if let Some(auth) = auth {
            req = req.header("authorization", auth);
        }
        if let Some(body) = body_json {
            req = req.body(body.to_string());
        }
        // 错误 detail 只含类别，不含 URL（URL 含 channel_id）。
        let detail = ApiError::transport_detail;
        let resp = req
            .send()
            .await
            .map_err(|e| ApiError::new(ApiErrorKind::Transport, detail(&e)))?;
        Ok(resp)
    }

    async fn request_json<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        auth: Option<&str>,
        body: Option<&impl Serialize>,
    ) -> Result<T, ApiError> {
        let body_json =
            match body {
                Some(b) => Some(serde_json::to_string(b).map_err(|_| {
                    ApiError::new(ApiErrorKind::Malformed, "failed to encode request")
                })?),
                None => None,
            };
        let resp = self.send(method, path, auth, body_json.as_deref()).await?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<T>()
                .await
                .map_err(|_| ApiError::new(ApiErrorKind::Malformed, "invalid response body"));
        }
        Err(map_error(resp).await)
    }

    async fn request_no_content(
        &self,
        method: reqwest::Method,
        path: &str,
        auth: &str,
    ) -> Result<(), ApiError> {
        let resp = self.send(method, path, Some(auth), None).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT || status.is_success() {
            return Ok(());
        }
        Err(map_error(resp).await)
    }
}

/// 非成功状态 → [`ApiError`]；读取 `Retry-After` 与错误体 `{error: CODE}`。
async fn map_error(resp: reqwest::Response) -> ApiError {
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let error_code = match resp.json::<serde_json::Value>().await {
        Ok(v) => v.get("error").and_then(|e| e.as_str()).map(str::to_string),
        Err(_) => None,
    };
    let kind = match status.as_u16() {
        400 => ApiErrorKind::BadRequest,
        401 => ApiErrorKind::InvalidSignature,
        403 => ApiErrorKind::Forbidden,
        404 => ApiErrorKind::NotFound,
        // COLLISION 是唯一可恢复的 409（撞码重试一次新码）。
        409 if error_code.as_deref() == Some("COLLISION") => ApiErrorKind::Collision,
        409 => ApiErrorKind::Conflict,
        413 => ApiErrorKind::TooLarge,
        429 => ApiErrorKind::RateLimited,
        500..=599 => ApiErrorKind::ServerError,
        _ => ApiErrorKind::Malformed,
    };
    let detail = error_code.unwrap_or_else(|| "request rejected".to_string());
    ApiError::with_retry_after(kind, detail, retry_after)
}

#[async_trait]
impl ApiClient for ReqwestClient {
    async fn create_channel(
        &self,
        body: CreateChannelBody,
        auth: &str,
    ) -> Result<CreateChannelResponse, ApiError> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/channels",
            Some(auth),
            Some(&body),
        )
        .await
    }

    async fn register_code(
        &self,
        channel_id: &str,
        body: RegisterCodeBody,
        auth: &str,
    ) -> Result<ExpiresAtResponse, ApiError> {
        let path = format!("/v1/channels/{channel_id}/code");
        self.request_json(reqwest::Method::POST, &path, Some(auth), Some(&body))
            .await
    }

    async fn lookup(&self, code_norm: &str) -> Result<LookupResponse, ApiError> {
        let path = format!("/v1/code/{code_norm}/lookup");
        self.request_json(
            reqwest::Method::POST,
            &path,
            None,
            None::<&serde_json::Value>,
        )
        .await
    }

    async fn join(
        &self,
        channel_id: &str,
        body: JoinBody,
        auth: &str,
    ) -> Result<StateResponse, ApiError> {
        let path = format!("/v1/channels/{channel_id}/join");
        self.request_json(reqwest::Method::POST, &path, Some(auth), Some(&body))
            .await
    }

    async fn handshake(
        &self,
        channel_id: &str,
        role: &str,
        body: HandshakeBody,
        auth: &str,
    ) -> Result<HandshakeResponse, ApiError> {
        let path = format!("/v1/channels/{channel_id}/handshake/{role}");
        self.request_json(reqwest::Method::POST, &path, Some(auth), Some(&body))
            .await
    }

    async fn upload_part(
        &self,
        channel_id: &str,
        body: UploadPartBody,
        auth: &str,
    ) -> Result<UploadPartResponse, ApiError> {
        let path = format!("/v1/channels/{channel_id}/parts");
        self.request_json(reqwest::Method::POST, &path, Some(auth), Some(&body))
            .await
    }

    async fn download_part(
        &self,
        channel_id: &str,
        part_index: u64,
        auth: &str,
    ) -> Result<Vec<u8>, ApiError> {
        let path = format!("/v1/channels/{channel_id}/parts/{part_index}");
        let resp = self
            .send(reqwest::Method::GET, &path, Some(auth), None)
            .await?;
        let status = resp.status();
        if status.is_success() {
            return resp.bytes().await.map(|b| b.to_vec()).map_err(|_| {
                ApiError::new(ApiErrorKind::Transport, "failed to read response body")
            });
        }
        Err(map_error(resp).await)
    }

    async fn confirm(&self, channel_id: &str, auth: &str) -> Result<(), ApiError> {
        let path = format!("/v1/channels/{channel_id}/confirm");
        self.request_no_content(reqwest::Method::POST, &path, auth)
            .await
    }

    async fn revoke(&self, channel_id: &str, auth: &str) -> Result<(), ApiError> {
        let path = format!("/v1/channels/{channel_id}/revoke");
        self.request_no_content(reqwest::Method::POST, &path, auth)
            .await
    }
}

/// base64url-no-pad 编码（DTO/签名字段）。
pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// base64url-no-pad 解码。
pub fn b64url_decode(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| anyhow::anyhow!("invalid base64url"))
}
