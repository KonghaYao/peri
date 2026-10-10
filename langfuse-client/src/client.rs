use std::time::Duration;

use base64::Engine;
use reqwest::Client;
use tracing::warn;

use crate::{
    error::LangfuseError,
    types::{ingestion_events_to_otel, IngestionEvent},
};

#[path = "client_export.rs"]
mod export;
#[path = "client_response.rs"]
mod response;
#[path = "client_retry.rs"]
mod retry;

pub use export::ExportConfig;

/// Langfuse OTLP 客户端，复用连接池和已编码请求，限制响应与累计发送时间。
#[derive(Clone)]
pub struct LangfuseClient {
    http: Client,
    base_url: String,
    auth_header: String,
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

        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
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

    /// 导出完整 OTLP 批次；部分拒收、无效响应或正文读取失败不会整批重发。
    /// 仅 429/502/503/504 和可恢复传输错误重试，次数及累计时间均有界。
    pub async fn ingest(&self, events: Vec<IngestionEvent>) -> Result<(), LangfuseError> {
        self.ingest_with_limit(events, self.export_config.max_request_bytes)
            .await
    }

    pub(crate) async fn ingest_with_limit(
        &self,
        events: Vec<IngestionEvent>,
        max_bytes: usize,
    ) -> Result<(), LangfuseError> {
        if events.is_empty() {
            return Ok(());
        }

        let deadline = peri_time::monotonic_now()
            .checked_add(self.export_config.retry_budget)
            .ok_or_else(retry::budget_error)?;
        let limit = max_bytes.min(self.export_config.max_request_bytes);
        export::preflight(&events, limit)?;
        let otel_payload = ingestion_events_to_otel(&events)?;
        let body = export::encode(&otel_payload, limit)?;
        drop(otel_payload);
        drop(events);
        let request = self
            .http
            .post(format!("{}/api/public/otel/v1/traces", self.base_url))
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .header("x-langfuse-ingestion-version", "4")
            .body(body)
            .build()
            .map_err(|_| LangfuseError::IngestionApi("OTLP request construction failed".into()))?;

        let mut attempt = 0;
        loop {
            if peri_time::monotonic_now() >= deadline {
                return Err(retry::budget_error());
            }
            let reusable = request.try_clone().ok_or_else(|| {
                LangfuseError::IngestionApi("OTLP encoded request cannot be reused".into())
            })?;
            let outcome = peri_time::timeout_at(deadline, self.send_once(reusable, attempt))
                .await
                .map_err(|_| retry::budget_error())?;
            let (error, retry_after) = match outcome {
                Attempt::Complete(result) => return result,
                Attempt::Retry(error, retry_after) => (error, retry_after),
            };
            if attempt >= self.max_retries {
                return Err(error);
            }
            let delay = retry::delay(&self.export_config, attempt, retry_after);
            let remaining = deadline.saturating_duration_since(peri_time::monotonic_now());
            if delay >= remaining {
                return Err(retry::budget_error());
            }
            attempt = attempt.saturating_add(1);
            warn!(
                error = %error,
                attempt,
                max_retries = self.max_retries,
                delay_ms = delay.as_millis(),
                "OTLP export transient failure; retrying"
            );
            peri_time::sleep(delay).await;
        }
    }

    async fn send_once(&self, request: reqwest::Request, attempt: usize) -> Attempt {
        let response = match self.http.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                let recoverable = error.is_timeout()
                    || error.is_connect()
                    || error.is_request()
                    || error.is_body();
                let error = LangfuseError::IngestionApi(format!(
                    "OTLP transport failed after {} retries",
                    attempt
                ));
                return if recoverable {
                    Attempt::Retry(error, None)
                } else {
                    Attempt::Complete(Err(error))
                };
            }
        };
        let status = response.status();
        if status == reqwest::StatusCode::OK {
            return Attempt::Complete(
                response::read_success(response, self.export_config.max_response_bytes).await,
            );
        }
        let error = LangfuseError::IngestionApi(format!(
            "OTLP ingestion HTTP {} after {} retries",
            status.as_u16(),
            attempt
        ));
        if matches!(status.as_u16(), 429 | 502 | 503 | 504) {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(retry::parse_retry_after);
            Attempt::Retry(error, retry_after)
        } else {
            Attempt::Complete(Err(error))
        }
    }
}

enum Attempt {
    Complete(Result<(), LangfuseError>),
    Retry(LangfuseError, Option<Duration>),
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
