use std::time::Duration;

use base64::Engine;
use reqwest::Client;
use tracing::warn;

use crate::{
    config::{Compression, TelemetryConfig},
    encoding::{BudgetedWriter, ExportContext},
    error::LangfuseError,
    memory::{MemoryBudget, GZIP_BILLING_BYTES},
    payload::ExportPayload,
    snapshot::{measure_event, TelemetryEvent},
    spool::{route_fingerprint, RouteFingerprint},
    types::IngestionEvent,
};

#[path = "client_body.rs"]
pub(crate) mod body;
#[path = "client_export.rs"]
pub(crate) mod export;
#[path = "client_response.rs"]
mod response;
#[path = "client_retry.rs"]
mod retry;
#[path = "client_send.rs"]
pub(crate) mod send;

pub use export::ExportConfig;
use send::{MemoryCheckpoint, SendOutcome};

/// Langfuse OTLP 客户端，复用连接池，按 byte 0 重开同一不可变正文并限制响应与累计发送时间。
#[derive(Clone)]
pub struct LangfuseClient {
    http: Client,
    base_url: String,
    auth_header: String,
    /// 部署 route 摘要（端点和凭据派生，不保存原值）；direct 路径的内部 payload 绑定同一身份。
    route: Option<RouteFingerprint>,
    max_retries: usize,
    export_config: ExportConfig,
}

impl LangfuseClient {
    /// 构造客户端；`max_retries` 不包含首次发送，0 表示不重试。
    pub fn new(public_key: &str, secret_key: &str, base_url: &str, max_retries: usize) -> Self {
        let credentials = format!("{}:{}", public_key, secret_key);
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .expect("failed to build reqwest client");
        let base_url = base_url.trim_end_matches('/').to_string();

        Self {
            route: route_fingerprint(&base_url, public_key, secret_key).ok(),
            http,
            base_url,
            auth_header: format!("Basic {}", encoded),
            max_retries,
            export_config: ExportConfig::default(),
        }
    }

    /// 从认证与端点配置构造，使用默认 HTTP 导出预算。
    pub fn from_config(config: &crate::config::ClientConfig, max_retries: usize) -> Self {
        Self::new(
            &config.public_key,
            &config.secret_key,
            &config.base_url,
            max_retries,
        )
    }

    /// 配置请求/响应字节预算、指数退避和累计发送期限。
    pub fn with_export_config(mut self, config: ExportConfig) -> Result<Self, LangfuseError> {
        config.validate()?;
        self.export_config = config;
        Ok(self)
    }

    /// 已生效的导出配置；批处理预算与拆批限额读同一份配置，不各自重解释来源。
    pub(crate) fn export_config(&self) -> &ExportConfig {
        &self.export_config
    }

    /// 导出完整 OTLP 批次；部分拒收、无效响应或正文读取失败不会整批重发。
    /// 仅 429/502/503/504 和可恢复传输错误重试，次数及累计时间均有界。
    pub async fn ingest(&self, events: Vec<IngestionEvent>) -> Result<(), LangfuseError> {
        self.ingest_with_limit(events, self.export_config.max_request_bytes)
            .await
    }

    /// direct ingest：同一编码入口只编码一次，再用同一 HTTP 实现发送；无持久 owner，
    /// checkpoint 只维护内存登记水位，正文始终驻留内存（不产生 File payload）。
    pub(crate) async fn ingest_with_limit(
        &self,
        events: Vec<IngestionEvent>,
        max_bytes: usize,
    ) -> Result<(), LangfuseError> {
        if events.is_empty() {
            return Ok(());
        }

        let payload = self.encode_direct(events, max_bytes)?;
        let mut checkpoint = MemoryCheckpoint::new();
        match self.send_payload(&payload, &mut checkpoint).await {
            SendOutcome::Delivered => Ok(()),
            SendOutcome::Retryable {
                error,
                retry_after,
                attempts,
            } => {
                warn!(error = %error, attempts, retry_after_ms = retry_after.as_millis(), "OTLP export retries exhausted");
                Err(error)
            }
            SendOutcome::Terminal { error, attempts } => {
                warn!(error = %error, attempts, "OTLP export failed permanently");
                Err(error)
            }
            SendOutcome::CheckpointFailed { error, attempts } => {
                warn!(error = %error, attempts, "OTLP export checkpoint failed");
                Err(error)
            }
        }
    }

    /// direct 路径的正文编码：走同一 `encode` 入口与同一 writer，只按最新事件预算绑定本地预算身份。
    ///
    /// 既有事件预检继续执行：事件数组 JSON 在**转换之前**先用同一上限计量，越限直接
    /// [`LangfuseError::PayloadTooLarge`]，不进入映射（因此也不受转换期身份校验影响）；
    /// 之后 writer 的 raw 守卫再对完整 OTLP 正文执行同一上限。
    /// retained 额度按本次**真实挂账量**取上界（见 [`direct_budget`]）：逐条实测的事件原文、
    /// writer 的一次性内存前缀与压缩账单，不做「限额×倍数」估算；不足时仍然返回
    /// [`LangfuseError::MemoryBudgetExceeded`] 而不是伪造正文。`spool=None` 时 writer 只用
    /// `max_wire_bytes` 作内存前缀上限，`TelemetryConfig` 的暂存阈值不参与这条路径。
    fn encode_direct(
        &self,
        events: Vec<IngestionEvent>,
        max_bytes: usize,
    ) -> Result<ExportPayload, LangfuseError> {
        let limit = max_bytes.min(self.export_config.max_request_bytes);
        export::preflight(&events, limit)?;
        let retained = direct_budget(&events, &self.export_config)?;
        let budget = MemoryBudget::new(retained)?;
        let telemetry = events
            .into_iter()
            .map(|event| TelemetryEvent::try_new(event, None, None, &budget))
            .collect::<Result<Vec<_>, _>>()?;
        let config = ExportConfig {
            max_request_bytes: limit,
            ..self.export_config.clone()
        };
        let writer = BudgetedWriter::new(
            &TelemetryConfig::default(),
            std::sync::Arc::clone(&budget),
            None,
            self.direct_context()?,
            self.export_config.max_wire_bytes,
            body::memory_registration(&budget)?,
        )?;
        export::encode(&telemetry, writer, &config)
    }

    /// direct payload 的冻结上下文：route 与客户端端点/凭据绑定，期限覆盖本次发送周期。
    fn direct_context(&self) -> Result<ExportContext, LangfuseError> {
        self.export_context(self.export_config.retry_budget)
    }

    /// 批处理 owner 的冻结上下文：同一 route 派生，期限为配置的暂存 TTL（契约 §5.2）。
    ///
    /// route 在构造期一次算出，这里只做 O(1) 组装；端点不接受时显式失败，不退化到空 route。
    pub(crate) fn export_context(&self, ttl: Duration) -> Result<ExportContext, LangfuseError> {
        let route = self.route.ok_or_else(|| {
            LangfuseError::Config(
                "langfuse endpoint must be a credential-free http(s) URL without query or fragment"
                    .into(),
            )
        })?;
        let accepted_at_ms = wall_ms();
        let window_ms = (ttl.as_millis() as i64).max(1);
        Ok(ExportContext {
            route,
            accepted_at_ms,
            expires_at_ms: accepted_at_ms.saturating_add(window_ms),
        })
    }

    /// 单个发送周期：每次 attempt 都 drain 上次读泵 → checkpoint → 从 byte 0 重建请求 →
    /// execute 与完整响应解析 → drain 读泵；只按状态码与传输错误分类决定是否重试。
    ///
    /// `attempts` 只来自 checkpoint 的累计持久水位（内存路径为登记水位），本函数不再相加。
    pub(crate) async fn send_payload(
        &self,
        payload: &ExportPayload,
        attempts: &mut dyn send::AttemptCheckpoint,
    ) -> SendOutcome {
        let deadline = match peri_time::monotonic_now().checked_add(self.export_config.retry_budget)
        {
            Some(deadline) => deadline,
            None => {
                return SendOutcome::Terminal {
                    error: retry::budget_error(),
                    attempts: 0,
                }
            }
        };
        let mut owner = send::BodyTaskOwner::new();
        let outcome = self
            .send_cycle(payload, attempts, deadline, &mut owner)
            .await;
        // 唯一出口收口：能确认 join 就 join；确认不了（期限已到、阻塞 IO 未返回）把读泵与
        // 工作区租约交给常驻回收任务，不丢弃仍持有读泵的 owner（契约 §3.5、D6）。
        send::retire(&mut owner, deadline).await;
        outcome
    }

    /// 单个发送周期的循环体；正文所有权永远留在调用方的 `owner` 里，
    /// 任何分支返回后都由 [`send_payload`] 统一收口，不在分支内偷偷丢句柄。
    async fn send_cycle(
        &self,
        payload: &ExportPayload,
        attempts: &mut dyn send::AttemptCheckpoint,
        deadline: peri_time::Instant,
        owner: &mut send::BodyTaskOwner,
    ) -> SendOutcome {
        let mut watermark = 0u64;
        let mut retries = 0usize;

        loop {
            if let Err(error) = send::settle(owner, deadline).await {
                return retryable(error, watermark);
            }
            if peri_time::monotonic_now() >= deadline {
                return retryable(retry::budget_error(), watermark);
            }
            watermark = match attempts.before_attempt().await {
                Ok(registered) => registered,
                Err(error) => {
                    return SendOutcome::CheckpointFailed {
                        error,
                        attempts: watermark,
                    }
                }
            };
            let request = match self.build_request(payload, owner).await {
                Ok(request) => request,
                // 正文打不开：保留对象与持久水位，交给下一次周期，不冒充已交付。
                Err(error) => return retryable(error, watermark),
            };
            let outcome =
                match peri_time::timeout_at(deadline, self.send_once(request, retries)).await {
                    Ok(outcome) => outcome,
                    Err(_elapsed) => return retryable(retry::budget_error(), watermark),
                };
            match outcome {
                Attempt::Delivered => {
                    return match send::settle(owner, deadline).await {
                        Ok(()) => SendOutcome::Delivered,
                        Err(error) => retryable(error, watermark),
                    };
                }
                Attempt::Terminal(error) => {
                    return SendOutcome::Terminal {
                        error,
                        attempts: watermark,
                    }
                }
                Attempt::Retry(error, retry_after) => {
                    if retries >= self.max_retries {
                        return SendOutcome::Retryable {
                            error,
                            retry_after: retry_after.unwrap_or(Duration::ZERO),
                            attempts: watermark,
                        };
                    }
                    let delay = retry::delay(&self.export_config, retries, retry_after);
                    let remaining = deadline.saturating_duration_since(peri_time::monotonic_now());
                    if delay >= remaining {
                        return retryable(retry::budget_error(), watermark);
                    }
                    retries = retries.saturating_add(1);
                    warn!(
                        error = %error,
                        attempt = retries,
                        max_retries = self.max_retries,
                        delay_ms = delay.as_millis(),
                        "OTLP export transient failure; retrying"
                    );
                    peri_time::sleep(delay).await;
                }
            }
        }
    }

    /// 单次 execute：429/502/503/504 与可恢复传输错误可重试，其余按终态返回；
    /// 200 委托完整响应解析，部分拒收、无效/过大/截断响应都不会整批重发。
    async fn send_once(&self, request: reqwest::Request, retries: usize) -> Attempt {
        let response = match self.http.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                let recoverable = error.is_timeout()
                    || error.is_connect()
                    || error.is_request()
                    || error.is_body();
                let error = LangfuseError::IngestionApi(format!(
                    "OTLP transport failed after {} retries",
                    retries
                ));
                return if recoverable {
                    Attempt::Retry(error, None)
                } else {
                    Attempt::Terminal(error)
                };
            }
        };
        let status = response.status();
        if status == reqwest::StatusCode::OK {
            return match response::read_success(response, self.export_config.max_response_bytes)
                .await
            {
                Ok(()) => Attempt::Delivered,
                Err(error) => Attempt::Terminal(error),
            };
        }
        let error = LangfuseError::IngestionApi(format!(
            "OTLP ingestion HTTP {} after {} retries",
            status.as_u16(),
            retries
        ));
        if matches!(status.as_u16(), 429 | 502 | 503 | 504) {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(retry::parse_retry_after);
            Attempt::Retry(error, retry_after)
        } else {
            Attempt::Terminal(error)
        }
    }
}

/// 可重试的失败：保留持久水位，由调用方（worker）决定下一周期。
fn retryable(error: LangfuseError, attempts: u64) -> SendOutcome {
    SendOutcome::Retryable {
        error,
        retry_after: Duration::ZERO,
        attempts,
    }
}

/// direct ingest 的 retained 上界（契约 §4）：本次真实挂账量之和，全部 checked。
///
/// 组成：逐条实测的事件原文（与 [`TelemetryEvent::try_new`] 的挂账同源）+ writer 构造时一次性
/// 预约的内存正文前缀 + 启用压缩时的 gzip 状态账单。任何溢出按配置错误上报，不用饱和运算
/// 掩盖，也不按「限额×倍数」估算。
fn direct_budget(events: &[IngestionEvent], config: &ExportConfig) -> Result<usize, LangfuseError> {
    let overflow = || LangfuseError::Config("direct ingest memory budget overflows".into());
    let events_total = events.iter().try_fold(0usize, |total, event| {
        total
            .checked_add(measure_event(event)?)
            .ok_or_else(overflow)
    })?;
    let compression = match config.compression {
        Compression::Gzip => GZIP_BILLING_BYTES,
        Compression::None => 0,
    };
    events_total
        .checked_add(config.max_wire_bytes)
        .and_then(|bytes| bytes.checked_add(compression))
        .ok_or_else(overflow)
}

/// 当前墙上时钟毫秒；非负。
fn wall_ms() -> i64 {
    use std::time::UNIX_EPOCH;
    peri_time::now_wall()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

enum Attempt {
    Delivered,
    Retry(LangfuseError, Option<Duration>),
    Terminal(LangfuseError),
}

#[cfg(test)]
#[path = "client_response_test.rs"]
mod response_tests;
#[cfg(test)]
#[path = "client_retry_test.rs"]
mod retry_tests;
#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
