use std::future::Future;
use std::time::{Duration, SystemTime};

use anyhow::Result;

use super::util::{auth_header_at, b64url, channel_hash, sha256_bytes};
use super::{ReceiverFlow, SenderFlow};
use crate::sync::http_client::{self, ApiError, ApiErrorKind, Backoff};
use crate::sync::limits;
use crate::sync::sync_code;

// ─── 码注册（30s epoch、撞码重试 1 次）─────────────────────────────────────

/// 注册本 epoch 的同步码（撞码重试 1 次）。
///
/// H2 复审修复：遇 403（channel 已离开 created 状态，如 receiver 已 join）返回
/// `Ok(None)` 表示"码使命已结束"——调用方停止注册/展示码并继续轮询 msg2，
/// 而不是传播错误退出整个 send 流程。
pub(super) async fn register_code_with_collision_retry(
    flow: &SenderFlow<'_>,
    channel_id: &str,
    epoch: u64,
) -> Result<Option<sync_code::SyncCode>> {
    let mut attempts = 0u32;
    loop {
        let code = sync_code::SyncCode::generate()?;
        let code_norm = code.normalized();
        let code_hash = b64url(&sha256_bytes(code_norm.as_bytes()));
        let body = http_client::RegisterCodeBody {
            code: code.display(),
            epoch,
        };
        let auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &flow.local.device_id,
            "code",
            &[channel_id, &epoch.to_string(), &code_hash],
        )?;
        match retry_429(flow.backoff, || {
            flow.client.register_code(channel_id, body.clone(), &auth)
        })
        .await
        {
            Ok(_) => return Ok(Some(code)),
            Err(e) if e.kind == ApiErrorKind::Forbidden => {
                tracing::debug!(
                    channel = %channel_hash(channel_id),
                    "code registration ended (channel no longer in created state)"
                );
                return Ok(None);
            }
            Err(e)
                if e.kind == ApiErrorKind::Collision
                    && attempts < limits::CODE_COLLISION_MAX_RETRIES =>
            {
                attempts += 1;
                tracing::warn!("sync code collision; retrying with a fresh code");
            }
            Err(e) => return Err(e.into()),
        }
    }
}

// ─── 下载重试（429 退避 + 404 在 channel 总超时内持续重试）───────────────────

/// 下载单个 part。429 由 [`retry_429`] 处理（独立预算）；404 表示尚未上传，
/// 在 `deadline`（channel 总超时）内持续重试（H5：不设固定次数）。
///
/// 二轮复审修复（High-1）：`auth` 在每次重试前重新构造——下载期间签名时间戳
/// 必须随墙钟刷新，否则 404 长重试跨 >300s 后必被服务端 401 截断。
pub(super) async fn fetch_part_with_retry(
    flow: &ReceiverFlow<'_>,
    channel_id: &str,
    part_index: u64,
    deadline: SystemTime,
    throttler: &mut PartThrottler,
) -> Result<Vec<u8>, ApiError> {
    loop {
        // Medium-2：每次请求前节流（≤1 req/s），避免 512 parts 突发撞 60/min。
        throttler.pace().await;
        let auth = auth_header_at(
            (flow.cfg.now)(),
            flow.store,
            &flow.local.device_id,
            "download",
            &[channel_id, &part_index.to_string()],
        )
        .map_err(|e| ApiError::new(ApiErrorKind::Transport, format!("auth: {e}")))?;
        match retry_429(flow.backoff, || {
            flow.client.download_part(channel_id, part_index, &auth)
        })
        .await
        {
            Ok(bytes) => {
                // C1：下载回来的 envelope 不得超 64KiB 密文上限（与 TS 同口径）。
                limits::validate_part_size(bytes.len()).map_err(|_| {
                    ApiError::new(
                        ApiErrorKind::TooLarge,
                        "downloaded part exceeds the 64 KiB limit",
                    )
                })?;
                return Ok(bytes);
            }
            Err(e) if e.kind == ApiErrorKind::NotFound && SystemTime::now() < deadline => {
                tracing::debug!(
                    part = part_index,
                    "part not available yet; retrying within channel timeout"
                );
                tokio::time::sleep(flow.cfg.poll_interval).await;
            }
            Err(e) => return Err(e),
        }
    }
}

// ─── 429 指数退避重试（Retry-After 优先）──────────────────────────────────

/// part 级请求节流器（Medium-2 复审修复）。
///
/// 服务端签名端点限流为 60/min（`limits.ts` 冻结值）；512 parts 全量传输若
/// 突发连发必吃满 429。客户端自节流保证任意 60s 窗口内请求 ≤60（≤1 req/s），
/// 429 仅在服务端滚动窗口边界抖动时兜底。
#[derive(Debug)]
pub(crate) struct PartThrottler {
    last_request: Option<SystemTime>,
    min_interval: Duration,
}

impl PartThrottler {
    pub(crate) fn new(min_interval: Duration) -> Self {
        Self {
            last_request: None,
            min_interval,
        }
    }

    /// 等待至距上次请求 ≥ `min_interval`。首次调用立即返回。
    pub(crate) async fn pace(&mut self) {
        if self.min_interval.is_zero() {
            return;
        }
        if let Some(last) = self.last_request {
            let elapsed = last.elapsed().unwrap_or_default();
            if elapsed < self.min_interval {
                let sleep = self.min_interval - elapsed;
                tokio::time::sleep(sleep).await;
            }
        }
        self.last_request = Some(SystemTime::now());
    }
}

pub(super) async fn retry_429<T, F, Fut>(backoff: &dyn Backoff, mut op: F) -> Result<T, ApiError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ApiError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Err(e) if e.kind == ApiErrorKind::RateLimited => {
                attempt += 1;
                if attempt > http_client::MAX_429_RETRIES {
                    return Err(e);
                }
                let delay = backoff.delay(attempt, e.retry_after_secs);
                tracing::debug!(attempt, ?delay, "rate limited; backing off");
                tokio::time::sleep(delay).await;
            }
            other => return other,
        }
    }
}
