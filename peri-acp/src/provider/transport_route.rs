//! 部署入口的显式模型传输路由；模型与会话执行层只消费捕获后的配置。

use anyhow::{Context, Result};
pub use peri_acp_types::model::ModelTransportRoute;
use url::Url;

pub const PROXY_ENV: &str = "PERI_MODEL_TRANSPORT_PROXY";
pub const RUN_ENV: &str = "PERI_REPLAY_RUN";

/// 缺省保持直连；任何不完整或非法的显式路由都使部署启动失败。
pub fn from_environment() -> Result<Option<ModelTransportRoute>> {
    let proxy = read_optional(PROXY_ENV)?;
    let run = read_optional(RUN_ENV)?;
    parse(proxy.as_deref(), run.as_deref())
}

fn read_optional(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Invalid {name}")),
    }
}

fn parse(proxy: Option<&str>, run: Option<&str>) -> Result<Option<ModelTransportRoute>> {
    match (proxy, run) {
        (None, None) => Ok(None),
        (Some(proxy), Some(run)) => {
            let url = Url::parse(proxy).with_context(|| format!("Invalid {PROXY_ENV}"))?;
            ModelTransportRoute::new(url, run)
                .map(Some)
                .with_context(|| format!("Invalid {PROXY_ENV}/{RUN_ENV} configuration"))
        }
        _ => anyhow::bail!("{PROXY_ENV} and {RUN_ENV} must be set together"),
    }
}

#[cfg(test)]
#[path = "transport_route_test.rs"]
mod tests;
