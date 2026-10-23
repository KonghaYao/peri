//! History V2 测试共用夹具：真实 SQLite 门面、真实 `ServerLoop` 与传输。
//!
//! 旧 V1 分页测试删除后，V2 的 adapter / initial / wire 用例仍需要同一份
//! 已注册会话与连接夹具；本模块是它们的父模块，子模块经 `use super::*`
//! 复用夹具与 `requests_test` 的装配 helper。

use super::*;
use crate::transport::AcpTransport;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::PeriCaps;
use tokio_util::sync::CancellationToken;

/// 真实 SQLite 门面 + 已注册会话（`register_session_with_history`：3 条 own 条目）。
///
/// `cfg` 是 `Arc`：分页用例既要把配置交给 `ServerLoop`（它按请求逐个派发），又要在运行期
/// 继续用同一份门面制造追加 / 再次 Full。
struct PageFixture {
    cfg: Arc<AcpServerConfig>,
    sessions: crate::host::SharedSessions,
    sid: String,
}

impl PageFixture {
    async fn new(tmp: &tempfile::TempDir) -> Self {
        let config = make_peri_config_with_provider(make_provider_config(
            "a",
            "openai",
            "unused-test-key",
            "gpt-4o",
        ));
        let provider = LlmProvider::from_config(&config).unwrap();
        let cfg = Arc::new(make_server_config(config, provider, tmp).await);
        let mut sessions = HashMap::new();
        let sid =
            register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
        Self {
            cfg,
            sessions: Arc::new(tokio::sync::Mutex::new(sessions)),
            sid,
        }
    }

    async fn own_ids(&self) -> Vec<MessageId> {
        self.cfg
            .session_resources
            .load_session_snapshot(&self.sid)
            .await
            .unwrap()
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect()
    }
}

/// 起一个真实 `ServerLoop`，返回客户端与（可断言的）共享会话集合。
async fn spawn_loop(
    cfg: Arc<AcpServerConfig>,
    sessions: crate::host::SharedSessions,
    connection_caps: Option<PeriCaps>,
) -> (
    Arc<crate::transport::mpsc::MpscClientTransport>,
    crate::host::SharedSessions,
    tokio::task::JoinHandle<()>,
) {
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let server: Arc<dyn AcpTransport> = Arc::new(server);
    spawn_loop_with_transport(cfg, sessions, connection_caps, client, server).await
}

/// 用**指定**的服务端 transport 起 `ServerLoop`：用例要控制出站通道时用。
async fn spawn_loop_with_transport(
    cfg: Arc<AcpServerConfig>,
    sessions: crate::host::SharedSessions,
    connection_caps: Option<PeriCaps>,
    client: crate::transport::mpsc::MpscClientTransport,
    server: Arc<dyn AcpTransport>,
) -> (
    Arc<crate::transport::mpsc::MpscClientTransport>,
    crate::host::SharedSessions,
    tokio::task::JoinHandle<()>,
) {
    let client = Arc::new(client);
    let observed = sessions.clone();
    let task = tokio::spawn(async move {
        let locks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
        let cont_tx = Arc::new(cont_tx);
        let mut context = crate::host::connection::ConnectionContext::new(false);
        if let Some(caps) = connection_caps {
            context.commit_initialize(caps);
        }
        let connection = Arc::new(tokio::sync::Mutex::new(context));
        let cancellation = CancellationToken::new();
        crate::host::server_loop::ServerLoop {
            transport: &server,
            cfg: &cfg,
            sessions: &sessions,
            prompt_locks: &locks,
            cont_tx: &cont_tx,
            connection: &connection,
            connection_cancellation: &cancellation,
        }
        .run()
        .await;
    });
    (client, observed, task)
}

/// 非阻塞取走客户端队列里**已经入队**的全部通知。
async fn s9c_drain_notifications(
    client: &crate::transport::mpsc::MpscClientTransport,
) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    loop {
        tokio::select! {
            biased;
            incoming = client.recv() => match incoming {
                Some(crate::transport::types::IncomingMessage::Notification { method, params }) => {
                    out.push((method, params))
                }
                Some(_) => {}
                None => break,
            },
            _ = std::future::ready(()) => break,
        }
    }
    out
}

// ── 子模块：History V2 的真实 SQLite / wire 用例（STD-SIZE-001）──────────────

#[path = "history_v2_adapter_test.rs"]
mod history_v2_adapter_tests;

#[path = "history_v2_adapter_inherited_test.rs"]
mod history_v2_adapter_inherited_tests;

#[path = "history_v2_load_test.rs"]
mod load_tests;

#[path = "history_v2_end_to_end_test.rs"]
mod end_to_end_tests;

#[path = "server_loop_history_test.rs"]
mod server_loop_history_tests;
