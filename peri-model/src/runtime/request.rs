use std::{collections::BTreeMap, fmt, sync::Arc};

use serde::{Serialize, Serializer};
use serde_json::Value;
use url::Url;

use crate::{ModelResult, ProviderProtocol, RetryConfig, RetryObserver};

const REDACTED_VALUE: &str = "[REDACTED]";
const TRUNCATED_VALUE: &str = "[TRUNCATED]";

/// 观测请求体的内部脱敏与截断策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservationConfig {
    max_field_chars: Option<usize>,
}

impl ObservationConfig {
    fn full_content() -> Self {
        Self {
            max_field_chars: None,
        }
    }
}

impl Default for ObservationConfig {
    fn default() -> Self {
        Self {
            max_field_chars: Some(4_096),
        }
    }
}

/// 运行时安全配置。配置仅由调用方显式构造，绝不读取环境变量。
#[derive(Default)]
pub struct ModelRuntimeConfig {
    observation: ObservationConfig,
    retry: RetryConfig,
    retry_observer: Option<Arc<dyn RetryObserver>>,
}

impl Clone for ModelRuntimeConfig {
    fn clone(&self) -> Self {
        Self {
            observation: self.observation.clone(),
            retry: self.retry.clone(),
            retry_observer: self.retry_observer.clone(),
        }
    }
}

impl fmt::Debug for ModelRuntimeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelRuntimeConfig")
            .field("observation", &self.observation)
            .field("retry", &self.retry)
            .field(
                "retry_observer",
                &self.retry_observer.as_ref().map(|_| "[REGISTERED]"),
            )
            .finish()
    }
}

impl ModelRuntimeConfig {
    /// 显式允许完整普通内容进入观测投影。
    ///
    /// 敏感键、非 ASCII 键和 data URI 始终脱敏；此选项只移除普通字段的长度限制。
    pub fn with_full_observation() -> Self {
        Self {
            observation: ObservationConfig::full_content(),
            retry: RetryConfig::default(),
            retry_observer: None,
        }
    }

    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// 注册 protocol-neutral 的 retry observer。该 observer 不会接收请求、响应或认证信息。
    pub fn with_retry_observer(mut self, observer: Arc<dyn RetryObserver>) -> Self {
        self.retry_observer = Some(observer);
        self
    }

    pub fn retry(&self) -> &RetryConfig {
        &self.retry
    }

    pub(crate) fn retry_observer(&self) -> Option<Arc<dyn RetryObserver>> {
        self.retry_observer.clone()
    }
}

/// 已脱敏的 provider 请求体。
pub struct ObservedProviderBody(Value);

impl ObservedProviderBody {
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl fmt::Debug for ObservedProviderBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ObservedProviderBody([REDACTED])")
    }
}

impl Serialize for ObservedProviderBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// 可供上层观测的安全请求投影。
///
/// 它不包含 headers、HTTP client、重试策略、认证信息或输入 endpoint 的路径。
#[derive(Serialize)]
pub struct PreparedModelRequest {
    protocol: ProviderProtocol,
    model_id: String,
    endpoint: Url,
    body: ObservedProviderBody,
    metadata: BTreeMap<String, Value>,
    redacted_paths: Vec<String>,
    truncated_paths: Vec<String>,
}

impl fmt::Debug for PreparedModelRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedModelRequest")
            .field("protocol", &self.protocol)
            .field("model_id", &self.model_id)
            .field("endpoint", &self.endpoint)
            .field("body", &"[REDACTED]")
            .field("metadata", &"[REDACTED]")
            .field("redacted_paths", &self.redacted_paths)
            .field("truncated_paths", &self.truncated_paths)
            .finish()
    }
}

impl PreparedModelRequest {
    /// 构造默认受限的安全观测投影。
    pub fn observe(
        protocol: ProviderProtocol,
        model_id: impl Into<String>,
        endpoint: Url,
        provider_body: Value,
        metadata: BTreeMap<String, Value>,
    ) -> ModelResult<Self> {
        Self::observe_with_runtime(
            protocol,
            model_id,
            endpoint,
            provider_body,
            metadata,
            &ModelRuntimeConfig::default(),
        )
    }

    pub(crate) fn observe_with_runtime(
        protocol: ProviderProtocol,
        model_id: impl Into<String>,
        endpoint: Url,
        provider_body: Value,
        metadata: BTreeMap<String, Value>,
        runtime: &ModelRuntimeConfig,
    ) -> ModelResult<Self> {
        let endpoint = observe_endpoint(endpoint)?;

        let mut redacted_paths = Vec::new();
        let mut truncated_paths = Vec::new();
        let body = observe_value(
            provider_body,
            "",
            &runtime.observation,
            &mut redacted_paths,
            &mut truncated_paths,
        );
        let metadata = observe_metadata(
            metadata,
            &runtime.observation,
            &mut redacted_paths,
            &mut truncated_paths,
        );

        Ok(Self {
            protocol,
            model_id: model_id.into(),
            endpoint,
            body: ObservedProviderBody(body),
            metadata,
            redacted_paths,
            truncated_paths,
        })
    }

    pub fn protocol(&self) -> &ProviderProtocol {
        &self.protocol
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn body(&self) -> &ObservedProviderBody {
        &self.body
    }

    pub fn metadata(&self) -> &BTreeMap<String, Value> {
        &self.metadata
    }

    pub fn redacted_paths(&self) -> &[String] {
        &self.redacted_paths
    }

    pub fn truncated_paths(&self) -> &[String] {
        &self.truncated_paths
    }
}

fn observe_endpoint(mut endpoint: Url) -> ModelResult<Url> {
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(crate::ModelError::protocol(
            crate::ProtocolErrorKind::InvalidEndpoint,
        ));
    }

    endpoint
        .set_username("")
        .map_err(|_| crate::ModelError::protocol(crate::ProtocolErrorKind::InvalidEndpoint))?;
    endpoint
        .set_password(None)
        .map_err(|_| crate::ModelError::protocol(crate::ProtocolErrorKind::InvalidEndpoint))?;
    endpoint.set_path("/[REDACTED]");
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

fn observe_metadata(
    metadata: BTreeMap<String, Value>,
    observation: &ObservationConfig,
    redacted_paths: &mut Vec<String>,
    truncated_paths: &mut Vec<String>,
) -> BTreeMap<String, Value> {
    metadata
        .into_iter()
        .filter_map(|(key, value)| {
            let path = format!("/metadata/{}", redacted_path_segment(&key));
            if is_sensitive_or_non_ascii_key(&key) {
                redacted_paths.push(path);
                None
            } else {
                Some((
                    key,
                    observe_value(value, &path, observation, redacted_paths, truncated_paths),
                ))
            }
        })
        .collect()
}

fn observe_value(
    value: Value,
    path: &str,
    observation: &ObservationConfig,
    redacted_paths: &mut Vec<String>,
    truncated_paths: &mut Vec<String>,
) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter_map(|(key, value)| {
                    let child_path = join_path(path, &key);
                    if is_sensitive_or_non_ascii_key(&key) {
                        redacted_paths.push(child_path);
                        None
                    } else {
                        Some((
                            key,
                            observe_value(
                                value,
                                &child_path,
                                observation,
                                redacted_paths,
                                truncated_paths,
                            ),
                        ))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    observe_value(
                        value,
                        &join_path(path, &index.to_string()),
                        observation,
                        redacted_paths,
                        truncated_paths,
                    )
                })
                .collect(),
        ),
        Value::String(value) if is_data_uri(&value) => {
            redacted_paths.push(path.to_owned());
            Value::String(REDACTED_VALUE.into())
        }
        Value::String(value) if exceeds_limit(&value, observation.max_field_chars) => {
            truncated_paths.push(path.to_owned());
            Value::String(TRUNCATED_VALUE.into())
        }
        value => value,
    }
}

fn exceeds_limit(value: &str, max_field_chars: Option<usize>) -> bool {
    max_field_chars.is_some_and(|limit| value.chars().count() > limit)
}

fn is_data_uri(value: &str) -> bool {
    value
        .trim_start()
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}

fn is_sensitive_or_non_ascii_key(key: &str) -> bool {
    !key.is_ascii() || is_sensitive_key(key)
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter_map(|byte| {
            byte.is_ascii_alphanumeric()
                .then_some(byte.to_ascii_lowercase())
        })
        .collect::<Vec<_>>();
    let normalized = String::from_utf8(normalized).expect("ASCII bytes are valid UTF-8");

    if normalized == "maxtokens"
        || normalized.starts_with("prompttokens")
        || normalized.starts_with("completiontokens")
    {
        return false;
    }

    // 复数 `*_tokens` 是模型请求中的计数/预算字段（max_tokens、budget_tokens、
    // input_tokens、cache_read_input_tokens 等），必须保留；单数 `*_token` 或裸
    // `token` 才是凭据载体（access_token、api_token、bearer_token 等）。
    // 注意：normalized 已移除下划线，不能使用 contains("token")——`budget_tokens`
    // 会因此被误删；也不能用 ends_with("_token")——`access_token` 归一化后是
    // `accesstoken`。
    if normalized.ends_with("tokens") {
        return false;
    }
    if normalized.ends_with("token") {
        return true;
    }

    [
        "headers",
        "credential",
        "credentials",
        "apikey",
        "authorization",
        "proxyauthorization",
        "cookie",
        "setcookie",
        "secret",
        "password",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn join_path(parent: &str, segment: &str) -> String {
    format!("{parent}/{}", redacted_path_segment(segment))
}

fn redacted_path_segment(segment: &str) -> String {
    if segment.is_ascii() {
        escape_path_segment(segment)
    } else {
        "[NON_ASCII_KEY]".into()
    }
}

fn escape_path_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
#[path = "request_test.rs"]
mod request_test;
