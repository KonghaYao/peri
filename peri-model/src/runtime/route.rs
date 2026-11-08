use url::Url;

use crate::{ModelError, ModelResult, ProtocolErrorKind};

/// 显式传输路由。逻辑 provider endpoint 与请求体仍由原配置构建。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTransportRoute {
    proxy_url: Url,
    run: String,
}

impl ModelTransportRoute {
    /// 代理使用独立 origin；实际请求保留原始 path/query，完整逻辑 URL 通过关联头传递。
    pub fn new(proxy_url: Url, run: impl Into<String>) -> ModelResult<Self> {
        let run = run.into();
        if !matches!(proxy_url.scheme(), "http" | "https")
            || proxy_url.host_str().is_none()
            || !proxy_url.username().is_empty()
            || proxy_url.password().is_some()
            || proxy_url.path() != "/"
            || proxy_url.query().is_some()
            || proxy_url.fragment().is_some()
        {
            return Err(
                ModelError::protocol(ProtocolErrorKind::InvalidEndpoint).with_message(
                    "Model transport route must be an HTTP(S) origin without credentials",
                ),
            );
        }
        if run.trim().is_empty() || run.len() > 256 {
            return Err(ModelError::protocol(ProtocolErrorKind::Other).with_message(
                "Model transport route requires a nonempty run of at most 256 bytes",
            ));
        }
        Ok(Self { proxy_url, run })
    }

    pub fn proxy_url(&self) -> &Url {
        &self.proxy_url
    }

    pub fn run(&self) -> &str {
        &self.run
    }
}
