//! MCP Apps TUI 开关与网关定位（纯解析，便于单测覆盖真值/缺项矩阵）。
//!
//! 两个环境变量缺一不可：`PERI_BROWSER_VIEW_URL` 指向网关控制面，
//! `PERI_BROWSER_VIEW_TOKEN` 是 view API 的 bearer token。token 只进请求头，
//! 不写日志、不落盘、不进 URL（[`ViewToken`] 的 `Debug` 恒为脱敏形态）。
//!
//! 开关本体复用 [`crate::launch`] 的 `PERI_MCP_APPS_TUI` 解析：只有显式真值
//! 才启用；URL / token 缺失时整条链路保持关闭，只打一行不含秘密的 warning。

use std::fmt;
use std::time::Duration;

use crate::launch::mcp_apps_tui_enabled;

/// 网关控制面地址变量（例如 `http://127.0.0.1:7391`）。
pub const VIEW_URL_ENV: &str = "PERI_BROWSER_VIEW_URL";
/// view API bearer token 变量（与网关 `--view-token` 同源）。
pub const VIEW_TOKEN_ENV: &str = "PERI_BROWSER_VIEW_TOKEN";
/// `apps/list` 轮询间隔；要求 ≥2s，避免控制面空转。
pub const DISCOVERY_INTERVAL: Duration = Duration::from_secs(3);

/// view API bearer token。
///
/// 故意不实现 `Display`/`Serialize`：token 只在 [`expose`](Self::expose) 的
/// 调用点（请求头）取出，`Debug` 恒为 `<redacted>`，避免误入日志或错误文本。
#[derive(Clone)]
pub struct ViewToken(String);

impl ViewToken {
    /// 空串/纯空白视为未配置（`None`）。
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_string()))
    }

    /// 唯一取值出口：调用方只能把它放进 `Authorization` 头。
    #[must_use]
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ViewToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ViewToken(<redacted>)")
    }
}

/// 启用的 Apps 视图配置（URL 已规范化、token 已脱敏包装）。
#[derive(Clone)]
pub struct AppsConfig {
    /// 规范化后的控制面基地址（无尾斜杠、无 userinfo）。
    pub view_url: String,
    pub token: ViewToken,
    pub discovery_interval: Duration,
}

impl fmt::Debug for AppsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppsConfig")
            .field("view_url", &self.view_url)
            .field("token", &self.token)
            .field("discovery_interval", &self.discovery_interval)
            .finish()
    }
}

/// 开关 + 环境变量的解析结果。
#[derive(Debug, Clone)]
pub enum AppsAvailability {
    /// 开关关闭：零网络、零轮询。
    Disabled,
    /// 开关开启但 URL/token 缺失：保持关闭，只打一行 warning。
    Unconfigured { missing: &'static str },
    /// 开关开启且配置有效。
    Enabled(AppsConfig),
}

impl AppsAvailability {
    #[must_use]
    pub fn enabled(&self) -> Option<&AppsConfig> {
        match self {
            Self::Enabled(config) => Some(config),
            Self::Disabled | Self::Unconfigured { .. } => None,
        }
    }
}

/// 纯解析：开关 + URL + token → 可用性。
///
/// URL 必须是 `http`/`https` 且不含 userinfo——避免把凭证塞进 URL（会被
/// 代理/日志记录）。非法 URL 按缺项处理（`missing = VIEW_URL_ENV`）。
#[must_use]
pub fn resolve(gate: bool, url: Option<&str>, token: Option<&str>) -> AppsAvailability {
    // 原始比较与规范化比较都在测试矩阵里锁定；生产路径只走规范化比较。
    let normalize = |raw: &str| -> Option<String> {
        let parsed = reqwest::Url::parse(raw.trim()).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return None;
        }
        let mut base = parsed.to_string();
        while base.ends_with('/') {
            base.pop();
        }
        (!base.is_empty()).then_some(base)
    };

    let normalized_url = url.map(str::trim).filter(|raw| !raw.is_empty());
    let token = token.and_then(ViewToken::new);

    if !gate {
        return AppsAvailability::Disabled;
    }
    if normalized_url.is_none() {
        return AppsAvailability::Unconfigured {
            missing: VIEW_URL_ENV,
        };
    }
    if token.is_none() {
        return AppsAvailability::Unconfigured {
            missing: VIEW_TOKEN_ENV,
        };
    }
    match (normalized_url.and_then(normalize), token) {
        (Some(view_url), Some(token)) => AppsAvailability::Enabled(AppsConfig {
            view_url,
            token,
            discovery_interval: DISCOVERY_INTERVAL,
        }),
        _ => AppsAvailability::Unconfigured {
            missing: VIEW_URL_ENV,
        },
    }
}

/// 从进程环境解析；格式不合法的 URL 视为缺项。
#[must_use]
pub fn from_env() -> AppsAvailability {
    resolve(
        mcp_apps_tui_enabled(),
        std::env::var(VIEW_URL_ENV).ok().as_deref(),
        std::env::var(VIEW_TOKEN_ENV).ok().as_deref(),
    )
}

/// 启动时唯一的 warning 出口：只说明缺哪个变量，不输出任何配置值。
pub(crate) fn warn_if_unconfigured(availability: &AppsAvailability) {
    if let AppsAvailability::Unconfigured { missing } = availability {
        tracing::warn!(
            env = missing,
            "MCP Apps TUI is on but the browser view gateway is not configured; app views stay off"
        );
    }
}

/// 测试用：显式注入原始环境值（不经 `std::env`，避免并行测试相互污染）。
#[cfg(test)]
#[must_use]
pub(crate) fn resolve_for_test(
    gate: &str,
    url: Option<&str>,
    token: Option<&str>,
) -> AppsAvailability {
    resolve(
        crate::launch::mcp_apps_tui_enabled_from(Some(gate)),
        url,
        token,
    )
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
