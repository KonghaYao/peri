//! Step 0 spike：rmcp 3.1.4 服务端推送面在 builtin duplex 链路的可用性（5 项验证）。
//!
//! 背景：Git Watch 下沉到 builtin `workspace` 实例后，唯一的**未运行时验证**假设是
//! 「服务端 `SubscriptionSink` 的推送能经同进程 duplex 链路按 `subscriptions/listen`
//! 的订阅 id 路由到客户端 `Subscription` 流」。本文件先用**真实** rmcp server +
//! **生产** client 握手段（`serve_client_auto`，与 builtin 生产链路同一入口）回答它，
//! 再在其上落 T4–T6 的 workspace 端到端用例。
//!
//! Step 0 探针（任一不成立 ⇒ 停止施工，见计划 §4）：
//! - ① `accepted_subscription_filter` 为 rmcp 默认实现（`None`）⇒ `peer.listen` 必须以
//!   `-32601` 失败（否则「未实现」与「未声明能力位」无法区分）。
//! - ①′ handler 返回 filter 但 `get_info` **未**声明 `resources.subscribe` ⇒ SDK 把
//!   handler filter 与 capabilities 求交后收窄为空 filter（能力位是必要条件）。
//! - ② 声明后 `listen` 成功；ack 的 filter 含请求 URI，且订阅 id = listen 请求 id。
//! - ③ `SubscriptionSink::notify_resource_updated(uri)` 到达客户端 `Subscription::next()`，
//!   携带同一订阅 id。
//! - ④ filter 外 URI 推送被 SDK 拒：`SubscriptionSendError::NotificationNotAccepted`。
//! - ⑤ 客户端 drop `Subscription` ⇒ 服务端 `cancelled()` 返回、`send` 返回
//!   `SubscriptionClosed`。
//!
//! 探针 handler 只声明订阅面（无工具、无资源正文），把实验变量限制在推送机制；
//! workspace 真实 handler 的线路证据见本文件 T4–T6。

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use rmcp::{
    model::{
        ErrorCode, GetMeta, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
        ServerNotification, SubscriptionFilter,
    },
    serve_server,
    service::{
        Peer, RoleClient, ServiceError, SubscriptionContext, SubscriptionSendError,
        SubscriptionSink,
    },
    ErrorData as McpError, ServerHandler,
};
use tokio::{task::JoinHandle, time::timeout};

use super::apps::McpCapabilityProfile;
use super::client::{serve_client_auto, McpServiceWrapper};

// ─── constants ────────────────────────────────────────────────────────────────

/// duplex 双向缓冲上界（与 `builtin_spike_test.rs` 同值：单帧 JSON-RPC 行只有几百字节）。
const DUPLEX_BUF: usize = 8 * 1024;
/// client 侧握手（`serve_client_auto` 内建）上界。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
/// server task 自然收敛的等待上界（有界，不无限 await）。
const SERVER_CONVERGE_TIMEOUT: Duration = Duration::from_millis(1000);
/// 单条异步观测（sink 注册 / cancelled / 通知到达）的等待上界。
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(2);

const SPIKE_SERVER_NAME: &str = "builtin-subscription-spike";
const SPIKE_SERVER_VERSION: &str = "0.0.1-spike";
/// 订阅目标 URI（与 `peri_mcp_workspace::GIT_REF_RESOURCE_URI` 同值：本文件是探针，
/// Step 0 阶段必须能在生产常量落地前编译，故保留字面量；T4–T6 另用生产常量断言绑定）。
const SPIKE_URI: &str = "workspace://git/ref";
/// 未采样时的资源正文（**冻结字面量**：与 `git_watch::UNSAMPLED_RESOURCE_TEXT` 同值，
/// 独立于服务端常量，防「服务端与断言一起漂移」）。
const UNSAMPLED_RESOURCE_TEXT: &str = "[Git watch] Repository ref has not been sampled yet.";
/// filter 外 URI（用于 ④）。
const OUT_OF_FILTER_URI: &str = "workspace://not-subscribed";

// ─── server fixture ───────────────────────────────────────────────────────────

/// 探针 server 的三种形态（本 spike 的实验变量）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterShape {
    /// rmcp 默认实现：`accepted_subscription_filter` 返回 `None`。
    Unimplemented,
    /// 返回交集 filter 且 `get_info` 声明 `resources.subscribe`。
    Advertised,
    /// 返回交集 filter 但 `get_info` **不**声明 `resources.subscribe`。
    FilterWithoutCapability,
}

/// 服务端观测点：listen 是否进入 / 是否收到取消 / 持有的 sink。
#[derive(Default)]
struct SubscriptionProbe {
    sink: parking_lot::Mutex<Option<SubscriptionSink>>,
    listen_entered: AtomicBool,
    cancelled: AtomicBool,
}

impl SubscriptionProbe {
    fn sink(&self) -> Option<SubscriptionSink> {
        self.sink.lock().clone()
    }

    fn listen_entered(&self) -> bool {
        self.listen_entered.load(Ordering::SeqCst)
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// 只声明订阅面的探针 handler。
#[derive(Clone)]
struct SpikeSubscriptionServer {
    shape: FilterShape,
    probe: Arc<SubscriptionProbe>,
}

impl SpikeSubscriptionServer {
    fn capabilities(&self) -> ServerCapabilities {
        match self.shape {
            FilterShape::Advertised => ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
            // 未实现面 / 缺能力位形态都不声明 subscribe。
            FilterShape::Unimplemented | FilterShape::FilterWithoutCapability => {
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_resources()
                    .build()
            }
        }
    }
}

impl ServerHandler for SpikeSubscriptionServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(self.capabilities())
            .with_server_info(Implementation::new(SPIKE_SERVER_NAME, SPIKE_SERVER_VERSION))
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        if self.shape == FilterShape::Unimplemented {
            return None;
        }
        let candidate = SubscriptionFilter::builder()
            .resource_subscriptions([SPIKE_URI])
            .build();
        Some(requested.intersection(&candidate))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        *self.probe.sink.lock() = Some(context.sink().clone());
        self.probe.listen_entered.store(true, Ordering::SeqCst);
        context.cancelled().await;
        self.probe.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }
}

// ─── wiring ───────────────────────────────────────────────────────────────────

/// server task 观测句柄（外层为 task join 结果，内层为 rmcp 服务退出原因）。
type ServerTask = JoinHandle<Result<rmcp::service::QuitReason, tokio::task::JoinError>>;

/// 一条已握手的同进程链路 + server task 观测句柄 + 服务端订阅观测。
struct SubscriptionPair {
    service: McpServiceWrapper,
    server_task: ServerTask,
    probe: Arc<SubscriptionProbe>,
}

impl SubscriptionPair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    /// 夹具收尾：关闭 client（释放 duplex 写半边），有界等待 server task；
    /// 未收敛时 abort，确保测试不留 orphan task。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        if timeout(SERVER_CONVERGE_TIMEOUT, &mut self.server_task)
            .await
            .is_err()
        {
            self.server_task.abort();
            let _ = self.server_task.await;
        }
    }
}

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

/// 装配一条同进程链路：server 半边是真实 `rmcp::serve_server`，client 半边走生产
/// `serve_client_auto`（Auto lifecycle ⇒ modern 协议，`subscriptions/listen` 才被受理）。
async fn connect(shape: FilterShape) -> SubscriptionPair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let probe = Arc::new(SubscriptionProbe::default());
    let server = SpikeSubscriptionServer {
        shape,
        probe: Arc::clone(&probe),
    };
    let server_task: ServerTask = tokio::spawn(async move {
        let running = match serve_server(server, server_io).await {
            Ok(running) => running,
            Err(error) => panic!("订阅 spike server 握手装配失败（{shape:?}）：{error}"),
        };
        running.waiting().await
    });
    let connect_result = serve_client_auto(
        tokio::io::split(client_io),
        None,
        None,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await;
    match connect_result {
        Ok(Ok(service)) => SubscriptionPair {
            service,
            server_task,
            probe,
        },
        Ok(Err(error)) => {
            server_task.abort();
            panic!("client 侧握手失败（{shape:?}）：{error}");
        }
        Err(elapsed) => {
            server_task.abort();
            panic!("client 侧握手超时（{shape:?}）：{elapsed}");
        }
    }
}

/// 请求 filter：恰好订阅 [`SPIKE_URI`]。
fn spike_filter() -> SubscriptionFilter {
    SubscriptionFilter::builder()
        .resource_subscriptions([SPIKE_URI])
        .build()
}

/// 断言本链路走 modern 协议（legacy 路径上 `subscriptions/listen` 一律 `-32601`）。
fn assert_modern_handshake(pair: &SubscriptionPair) {
    let info = pair
        .peer()
        .peer_info()
        .unwrap_or_else(|| panic!("握手后 peer_info 为 None（订阅面要求 modern 协议）"));
    assert_eq!(
        info.protocol_version,
        ProtocolVersion::V_2026_07_28,
        "builtin duplex 链路必须协商为 2026-07-28 才会受理 subscriptions/listen"
    );
}

// ─── ① 未实现面 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn step0_1_unimplemented_filter_listen_is_method_not_found() {
    let pair = connect(FilterShape::Unimplemented).await;
    assert_modern_handshake(&pair);

    let result = pair.peer().listen(spike_filter()).await;
    match result {
        Err(ServiceError::McpError(error)) => assert_eq!(
            error.code,
            ErrorCode::METHOD_NOT_FOUND,
            "① handler 未实现 subscriptions/listen 时应回 -32601，实际 {error:?}"
        ),
        other => panic!("① 期望 -32601 失败，实际 {other:?}"),
    }
    assert!(
        !pair.probe.listen_entered(),
        "① 未实现面不应进入 listen 回调"
    );
    pair.shutdown().await;
}

// ─── ①′ 能力位求交 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn step0_1b_missing_subscribe_capability_narrows_filter_to_empty() {
    let pair = connect(FilterShape::FilterWithoutCapability).await;
    assert_modern_handshake(&pair);

    let subscription = pair
        .peer()
        .listen(spike_filter())
        .await
        .expect("①′ 返回了 filter 实现时 listen 仍应成功（求交发生在受理阶段）");
    assert!(
        subscription.acknowledged().resource_subscriptions.is_none(),
        "①′ 未声明 resources.subscribe 时 SDK 必须把 filter 求交为空，实际 {:?}",
        subscription.acknowledged()
    );
    pair.shutdown().await;
}

// ─── ② + ③ ack 与推送 ────────────────────────────────────────────────────────

#[tokio::test]
async fn step0_2_3_ack_and_push_reach_client_with_subscription_id() {
    let pair = connect(FilterShape::Advertised).await;
    assert_modern_handshake(&pair);

    let mut subscription = pair
        .peer()
        .listen(spike_filter())
        .await
        .expect("② listen 成功");
    let subscription_id = subscription.id().to_string();
    assert_eq!(
        subscription.acknowledged().resource_subscriptions,
        Some(vec![SPIKE_URI.to_string()]),
        "② ack 必须保留请求的 URI"
    );

    wait_until("② 服务端进入 listen 回调", || {
        pair.probe.listen_entered()
    })
    .await;
    let sink = pair.probe.sink().expect("listen 回调必须登记 sink");
    assert_eq!(
        sink.id().to_string(),
        subscription_id,
        "② 服务端 sink 的订阅 id 必须等于 listen 请求 id"
    );

    // ③ 推送命中 filter 内的 URI。
    sink.notify_resource_updated(SPIKE_URI)
        .await
        .expect("③ filter 内 URI 推送必须被接受");
    let notification = timeout(OBSERVE_TIMEOUT, subscription.next())
        .await
        .expect("③ 推送必须在期限内到达客户端")
        .expect("③ 订阅流不应出错")
        .expect("③ 推送必须产生一条通知");
    match notification {
        ServerNotification::ResourceUpdatedNotification(ref update) => {
            assert_eq!(update.params.uri, SPIKE_URI, "③ 通知 URI 必须一致");
            // 订阅 id 由 SDK 写在 `extensions` 层的 `_meta`（出站 `get_meta_mut()` 写入、
            // 入站同样落在 `extensions`）；`params.meta` 是本仓库出站才填充的 typed 字段，
            // 入站恒为 `None`——**不要**用 `params.meta` 读订阅 id。
            assert_eq!(
                notification
                    .get_meta()
                    .subscription_id()
                    .map(|id| id.to_string()),
                Some(subscription_id),
                "③ 通知必须携带订阅 id 供路由"
            );
        }
        other => panic!("③ 期望 ResourceUpdatedNotification，实际 {other:?}"),
    }

    pair.shutdown().await;
}

// ─── ④ filter 外推送被拒 ─────────────────────────────────────────────────────

#[tokio::test]
async fn step0_4_out_of_filter_push_is_rejected() {
    let pair = connect(FilterShape::Advertised).await;
    assert_modern_handshake(&pair);

    let _subscription = pair
        .peer()
        .listen(spike_filter())
        .await
        .expect("listen 成功");
    wait_until("④ sink 登记", || pair.probe.listen_entered()).await;
    let sink = pair.probe.sink().expect("listen 回调必须登记 sink");

    match sink.notify_resource_updated(OUT_OF_FILTER_URI).await {
        Err(SubscriptionSendError::NotificationNotAccepted(reason)) => assert_eq!(
            reason, "notifications/resources/updated",
            "④ 拒绝原因应为该通知类别"
        ),
        other => panic!("④ filter 外 URI 必须被 SDK 拒，实际 {other:?}"),
    }

    pair.shutdown().await;
}

// ─── ⑤ drop 取消 + 关闭后 send 失败 ──────────────────────────────────────────

#[tokio::test]
async fn step0_5_drop_cancels_server_listen_and_closes_sink() {
    let pair = connect(FilterShape::Advertised).await;
    assert_modern_handshake(&pair);

    let subscription = pair
        .peer()
        .listen(spike_filter())
        .await
        .expect("listen 成功");
    wait_until("⑤ 服务端进入 listen 回调", || {
        pair.probe.listen_entered()
    })
    .await;
    let sink = pair.probe.sink().expect("listen 回调必须登记 sink");

    // drop 客户端句柄 ⇒ 发送显式取消。
    drop(subscription);
    wait_until("⑤ 服务端 cancelled() 返回", || {
        pair.probe.cancelled()
    })
    .await;

    match sink.notify_resource_updated(SPIKE_URI).await {
        Err(SubscriptionSendError::SubscriptionClosed) => {}
        other => panic!("⑤ 订阅关闭后 send 必须回 SubscriptionClosed，实际 {other:?}"),
    }

    pair.shutdown().await;
}

// T4–T7b（真实 workspace handler 的线路证据）拆到子模块：单文件行数上限 STD-SIZE-001。
// 子模块用 `super::*` 复用本文件的探针夹具（`SubscriptionProbe` / `SpikeSubscriptionServer`
// 等），模块路径仍是 `mcp::builtin_subscription_wire_tests::workspace_wire_tests`，
// `cargo test ... -- mcp::builtin_subscription_wire` 过滤器对两者都命中。
#[path = "builtin_subscription_workspace_wire_test.rs"]
mod workspace_wire_tests;
