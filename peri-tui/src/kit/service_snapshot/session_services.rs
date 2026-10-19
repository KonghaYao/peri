//! 会话服务协调器：查询 Plugin、MCP 和 Cron，并隔离各服务的本次错误。
//!
//! 同一 session-generation 保留 last-good；代次变化由 reset_services 清理缓存。
//! 无可查询会话时只允许带启动提示的静态 hooks/plugins，不直接发布任何 atom。

use serde::Deserialize;
use serde_json::json;

use crate::{
    acp_client::AcpTuiClient,
    kit::atoms::{
        CronJobSummary, HookSummary, McpInitPhase, McpServerSummary, McpStatusSnapshot,
        PluginSummary,
    },
};

pub(super) struct SessionServices {
    pub hooks: Vec<HookSummary>,
    pub plugins: Vec<PluginSummary>,
    pub mcp_servers: Vec<McpServerSummary>,
    pub mcp: McpStatusSnapshot,
    pub cron_jobs: Vec<CronJobSummary>,
}

impl Default for SessionServices {
    fn default() -> Self {
        Self {
            hooks: Vec::new(),
            plugins: Vec::new(),
            mcp_servers: Vec::new(),
            mcp: McpStatusSnapshot {
                init_phase: McpInitPhase::Failed,
                ..Default::default()
            },
            cron_jobs: Vec::new(),
        }
    }
}

pub(super) struct ServiceObservation {
    pub hooks: Vec<HookSummary>,
    pub plugins: Vec<PluginSummary>,
    pub error: Option<String>,
}

impl SessionServices {
    pub(super) async fn refresh(
        &mut self,
        source: &super::SnapshotSource,
        session_id: &str,
        identity: &Option<(String, u64)>,
    ) -> ServiceObservation {
        if let Some(client) = &source.client
            && !session_id.is_empty()
            && identity.as_ref().is_some_and(|owner| owner.0 == session_id)
        {
            let error = query(client, session_id, self).await;
            return ServiceObservation {
                hooks: self.hooks.clone(),
                plugins: self.plugins.clone(),
                error,
            };
        }
        *self = Self::default();
        let error = Some(crate::i18n::tr(if session_id.is_empty() {
            "service-projection-startup"
        } else if source.client.is_none() {
            "service-projection-no-client"
        } else {
            "service-projection-session-unavailable"
        }));
        let (hooks, plugins) = if session_id.is_empty() {
            (source.hooks.clone(), source.plugins.clone())
        } else {
            (Vec::new(), Vec::new())
        };
        ServiceObservation {
            hooks,
            plugins,
            error,
        }
    }
}

#[derive(Deserialize)]
struct Plugins {
    hooks: Vec<HookSummary>,
    plugins: Vec<PluginSummary>,
}

#[derive(Deserialize)]
struct McpServers {
    servers: Vec<McpServer>,
}

#[derive(Deserialize)]
struct CronJobs {
    jobs: Vec<peri_acp_types::cron::CronTaskInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpServer {
    name: String,
    transport: String,
    connection_status: String,
    oauth_status: String,
    tools_count: usize,
    /// 工具有界投影；旧 host 不返回时为空（计数仍由 toolsCount 承载）。
    #[serde(default)]
    tools: Vec<McpToolWire>,
    /// 资源有界投影；旧 host 不返回时为空。
    #[serde(default)]
    resources: Vec<McpResourceWire>,
    /// 资源全量计数；旧 host 缺省为 0。
    #[serde(default)]
    resources_count: usize,
    /// 订阅配置；旧 host 不返回时为 None。
    #[serde(default)]
    subscriptions: Option<McpSubscriptionsWire>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    connected_at: Option<String>,
    #[serde(default)]
    protocol_version: Option<String>,
    /// failed 状态的一行安全失败摘要（`mcp/list` 有界投影）；详情视图展示。
    #[serde(default)]
    error_summary: Option<String>,
    /// 服务器 URL（HTTP 传输）；详情视图展示。
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpToolWire {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpResourceWire {
    uri: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpSubscriptionsWire {
    #[serde(default)]
    resources: Vec<String>,
    #[serde(default)]
    tools_list_changed: bool,
    #[serde(default)]
    prompts_list_changed: bool,
    #[serde(default)]
    resources_list_changed: bool,
}

pub(super) async fn query(
    client: &AcpTuiClient,
    session_id: &str,
    result: &mut SessionServices,
) -> Option<String> {
    let params = json!({"sessionId":session_id});
    let (plugins, mcp, cron) = tokio::join!(
        request(client, "plugin/list", params.clone()),
        request(client, "mcp/list", params.clone()),
        request(client, "cron/list", params),
    );
    let mut errors = Vec::new();
    match plugins.and_then(|value| {
        serde_json::from_value::<Plugins>(value)
            .map_err(|error| peri_acp::transport::types::AcpError::new(-32603, error.to_string()))
    }) {
        Ok(plugins) => {
            result.plugins = plugins.plugins;
            result.hooks = plugins
                .hooks
                .into_iter()
                .map(|mut hook| {
                    hook.event.make_ascii_lowercase();
                    hook
                })
                .collect();
        }
        Err(error) => errors.push(format!("plugin/list: {error}")),
    }
    match mcp.and_then(|value| {
        serde_json::from_value::<McpServers>(value)
            .map_err(|error| peri_acp::transport::types::AcpError::new(-32603, error.to_string()))
    }) {
        Ok(servers) => {
            result.mcp = McpStatusSnapshot {
                total: servers.servers.len(),
                connected: servers
                    .servers
                    .iter()
                    .filter(|server| server.connection_status == "connected")
                    .count(),
                init_phase: McpInitPhase::Ready,
            };
            result.mcp_servers = servers
                .servers
                .into_iter()
                .map(|server| McpServerSummary {
                    name: server.name,
                    version: server.version,
                    connected_at: server.connected_at,
                    protocol_version: server.protocol_version,
                    transport: server.transport,
                    status: server.connection_status,
                    needs_auth: server.oauth_status == "needs_authorization",
                    tools_count: server.tools_count,
                    tools: server
                        .tools
                        .into_iter()
                        .map(|tool| crate::kit::atoms::McpToolSummary {
                            name: tool.name,
                            description: tool.description,
                        })
                        .collect(),
                    resources: server
                        .resources
                        .into_iter()
                        .map(|resource| crate::kit::atoms::McpResourceSummary {
                            uri: resource.uri,
                            name: resource.name,
                            description: resource.description,
                        })
                        .collect(),
                    resources_count: server.resources_count,
                    subscriptions: server.subscriptions.map(|subscriptions| {
                        crate::kit::atoms::McpSubscriptionsSummary {
                            resources: subscriptions.resources,
                            tools_list_changed: subscriptions.tools_list_changed,
                            prompts_list_changed: subscriptions.prompts_list_changed,
                            resources_list_changed: subscriptions.resources_list_changed,
                        }
                    }),
                    error_summary: server.error_summary,
                    url: server.url,
                    ..McpServerSummary::default()
                })
                .collect();
        }
        Err(error) => {
            result.mcp.init_phase = McpInitPhase::Failed;
            errors.push(format!("mcp/list: {error}"));
        }
    }
    match cron.and_then(|value| {
        serde_json::from_value::<CronJobs>(value)
            .map_err(|error| peri_acp::transport::types::AcpError::new(-32603, error.to_string()))
    }) {
        Ok(jobs) => {
            result.cron_jobs = jobs
                .jobs
                .into_iter()
                .map(|job| CronJobSummary {
                    id: job.id,
                    expression: job.expression,
                    prompt: job.prompt,
                    enabled: job.enabled,
                    next_fire: job.next_fire,
                })
                .collect()
        }
        Err(error) => errors.push(format!("cron/list: {error}")),
    }
    if errors.is_empty() {
        None
    } else {
        let detail = errors.join("; ");
        tracing::warn!(session_id, %detail, "session service projection failed; retaining same-generation successful data");
        Some(crate::i18n::tr_args(
            "service-projection-failed",
            &[("detail".into(), fluent_bundle::FluentValue::from(detail))],
        ))
    }
}

async fn request(
    client: &AcpTuiClient,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, peri_acp::transport::types::AcpError> {
    peri_time::timeout(
        std::time::Duration::from_secs(10),
        client.send_raw_request(method, params),
    )
    .await
    .map_err(|_| {
        peri_acp::transport::types::AcpError::new(-32603, "service projection request timed out")
    })?
}

#[cfg(test)]
#[path = "session_services_test.rs"]
mod tests;
