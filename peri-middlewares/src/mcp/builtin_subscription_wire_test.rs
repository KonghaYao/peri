//! Git Watch 下沉（builtin workspace 订阅回传）的线路级验收：T4–T7b 见子模块
//! `builtin_subscription_workspace_wire_test.rs`（真实 `WorkspaceMcpServer` + 生产链路装配）。
//!
//! 本文件承载子模块复用的**探针夹具**：一个只声明订阅面（无工具、无资源正文，
//! `resources/read` 走 rmcp 默认 ⇒ `-32601`）的 `rmcp::ServerHandler`，配
//! `SubscriptionProbe` 服务端观测点。T7b 用它接线进 pool 的生产订阅消费循环，
//! 验证「回读失败 ⇒ 回退通用提醒」路径。

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use rmcp::{
    model::{Implementation, ServerCapabilities, ServerConfig, SubscriptionFilter},
    service::{Peer, RoleClient, SubscriptionContext, SubscriptionSink},
    ErrorData as McpError, ServerHandler,
};
use tokio::time::timeout;

use super::apps::McpCapabilityProfile;
use super::client::{serve_client_auto, McpServiceWrapper};

// ─── constants ────────────────────────────────────────────────────────────────

/// client 侧握手（`serve_client_auto` 内建）上界。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
/// 单条异步观测（sink 注册 / 通知到达）的等待上界。
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(2);

const SPIKE_SERVER_NAME: &str = "builtin-subscription-spike";
const SPIKE_SERVER_VERSION: &str = "0.0.1-spike";
/// 订阅目标 URI（与 `peri_mcp_workspace::GIT_REF_RESOURCE_URI` 同值：本文件是探针夹具，
/// 独立于生产常量，防「服务端与断言一起漂移」；子模块另用生产常量断言绑定）。
const SPIKE_URI: &str = "workspace://git/ref";
/// 未采样时的资源正文（**冻结字面量**：与 `git_watch::UNSAMPLED_RESOURCE_TEXT` 同值，
/// 独立于服务端常量，防「服务端与断言一起漂移」）。
const UNSAMPLED_RESOURCE_TEXT: &str = "[Git watch] Repository ref has not been sampled yet.";

// ─── server fixture ───────────────────────────────────────────────────────────

/// 服务端观测点：listen 是否进入 / 持有的 sink。
#[derive(Default)]
struct SubscriptionProbe {
    sink: parking_lot::Mutex<Option<SubscriptionSink>>,
    listen_entered: AtomicBool,
}

impl SubscriptionProbe {
    fn sink(&self) -> Option<SubscriptionSink> {
        self.sink.lock().clone()
    }

    fn listen_entered(&self) -> bool {
        self.listen_entered.load(Ordering::SeqCst)
    }
}

/// 只声明订阅面的探针 handler。
#[derive(Clone)]
struct SpikeSubscriptionServer {
    probe: Arc<SubscriptionProbe>,
}

impl SpikeSubscriptionServer {
    fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .build()
    }
}

impl ServerHandler for SpikeSubscriptionServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(self.capabilities())
            .with_server_info(Implementation::new(SPIKE_SERVER_NAME, SPIKE_SERVER_VERSION))
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let candidate = SubscriptionFilter::builder()
            .resource_subscriptions([SPIKE_URI])
            .build();
        Some(requested.intersection(&candidate))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        *self.probe.sink.lock() = Some(context.sink().clone());
        self.probe.listen_entered.store(true, Ordering::SeqCst);
        context.cancelled().await;
        Ok(())
    }
}

// ─── wiring ───────────────────────────────────────────────────────────────────

/// 有界等待一个异步观测成立；超时即 panic（携带 label 便于定位）。
async fn wait_until(label: &str, mut cond: impl FnMut() -> bool) {
    let observed = timeout(OBSERVE_TIMEOUT, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(observed.is_ok(), "等待超时：{label}");
}

// T4–T7b（真实 workspace handler 的线路证据）拆到子模块：单文件行数上限 STD-SIZE-001。
// 子模块用 `super::*` 复用本文件的探针夹具（`SubscriptionProbe` / `SpikeSubscriptionServer`
// 等），模块路径仍是 `mcp::builtin_subscription_wire_tests::workspace_wire_tests`，
// `cargo test ... -- mcp::builtin_subscription_wire` 过滤器对两者都命中。
#[path = "builtin_subscription_workspace_wire_test.rs"]
mod workspace_wire_tests;
