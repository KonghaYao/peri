use std::time::Duration;

use base64::Engine;
use reqwest::Client;
use tracing::warn;

use crate::{
    error::LangfuseError,
    types::{ingestion_events_to_otel, IngestionEvent},
};

/// Langfuse OTLP Ingestion 客户端
///
/// 通过 OpenTelemetry OTLP 端点（/api/public/otel/v1/traces）发送追踪数据。
/// 持有 reqwest::Client（复用连接池），封装认证、请求构建、重试逻辑。
#[derive(Clone)]
pub struct LangfuseClient {
    http: Client,
    base_url: String,
    auth_header: String,
    max_retries: usize,
}

impl LangfuseClient {
    /// 构造 LangfuseClient
    ///
    /// - `public_key`: Langfuse 公钥
    /// - `secret_key`: Langfuse 秘钥
    /// - `base_url`: Langfuse 服务地址（如 "https://cloud.langfuse.com"）
    /// - `max_retries`: 网络错误最大重试次数（0 = 不重试）
    pub fn new(public_key: &str, secret_key: &str, base_url: &str, max_retries: usize) -> Self {
        let credentials = format!("{}:{}", public_key, secret_key);
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        let auth_header = format!("Basic {}", encoded);

        // 配置 reqwest Client 超时：连接超时 5s，请求超时 30s
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");

        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_header,
            max_retries,
        }
    }

    /// 从 ClientConfig 构造（便捷方法）
    pub fn from_config(config: &crate::config::ClientConfig, max_retries: usize) -> Self {
        Self::new(
            &config.public_key,
            &config.secret_key,
            &config.base_url,
            max_retries,
        )
    }

    /// 发送一批事件到 Langfuse OTLP 端点
    ///
    /// POST /api/public/otel/v1/traces
    /// 将 IngestionEvent 批量转换为 OTLP resourceSpans 格式发送。
    /// Headers:
    ///   - Authorization: Basic {base64(public_key:secret_key)}
    ///   - Content-Type: application/json
    ///   - x-langfuse-ingestion-version: 4
    ///
    /// 响应: 200 OK（空对象）表示成功
    /// 错误重试: 网络错误和 5xx 自动重试 max_retries 次，指数退避（1s, 2s, 4s...）
    /// 4xx 错误不重试，直接返回 LangfuseError::IngestionApi
    pub async fn ingest(&self, events: Vec<IngestionEvent>) -> Result<(), LangfuseError> {
        if events.is_empty() {
            return Ok(());
        }

        let url = format!("{}/api/public/otel/v1/traces", self.base_url);
        let otel_payload = ingestion_events_to_otel(&events);

        let mut attempt = 0;
        loop {
            let result = self
                .http
                .post(&url)
                .header("Authorization", &self.auth_header)
                .header("Content-Type", "application/json")
                .header("x-langfuse-ingestion-version", "4")
                .json(&otel_payload)
                .send()
                .await;

            match result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        if let Err(e) = response.bytes().await {
                            warn!("OTLP ingestion response body read failed: {}", e);
                        }
                        return Ok(());
                    } else if status.is_client_error() {
                        let error_text = response.text().await.unwrap_or_default();
                        return Err(LangfuseError::IngestionApi(format!(
                            "OTLP ingestion HTTP {}: {}",
                            status, error_text
                        )));
                    } else {
                        let error_text = response.text().await.unwrap_or_default();
                        if attempt < self.max_retries {
                            attempt += 1;
                            let delay = Duration::from_secs(1 << (attempt - 1));
                            warn!(
                                "OTLP ingestion server error (attempt {}/{}), retrying in {:?}: HTTP {} {}",
                                attempt, self.max_retries, delay, status, error_text
                            );
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Err(LangfuseError::IngestionApi(format!(
                            "OTLP ingestion HTTP {} after {} retries: {}",
                            status, self.max_retries, error_text
                        )));
                    }
                }
                Err(e) => {
                    if attempt < self.max_retries {
                        attempt += 1;
                        let delay = Duration::from_secs(1 << (attempt - 1));
                        warn!(
                            "OTLP ingestion network error (attempt {}/{}), retrying in {:?}: {}",
                            attempt, self.max_retries, delay, e
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(LangfuseError::Http(e));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
