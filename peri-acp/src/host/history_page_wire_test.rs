//! 历史页与初始回放的 wire 级行为（REQ §5.1~§5.3、验收 A08/A09/A15/A20/A21/A22/A25）。
//!
//! 走真实的 `ServerLoop` + MPSC transport：模式分流、执行锁与响应装配是
//! `dispatch_request` 的职责，只测下层函数会漏掉「分页是否被执行锁挡住」。
//!
//! 本模块是按职责拆出的**父模块**（STD-SIZE-001：单文件 ≤1000 行）：只放连接/存储夹具、
//! 跨文件共享的判据 helper 与子模块挂载；用例本身在同目录的 `history_page_*_test.rs`：
//! 首屏与初始游标契约、页链与合并、拒绝路径、rewind 与过期游标、能力门控与私有字段。
//! 子模块经 `use super::*` 复用这里的夹具与 helper，不各自另造连接或存储。

use super::*;
use crate::host::history_page;
use crate::transport::AcpTransport;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::store::{CompactionChange, MessageFlags};
use peri_acp_types::PeriCaps;
use tokio_util::sync::CancellationToken;

// ── 子模块：按职责拆分（每个文件 ≤1000 行）──────────────────────────────────

/// 首屏 / 初始游标契约：初始游标锚点、首屏身份与页可比、初始回放失败不假造游标。
#[path = "history_page_initial_cursor_test.rs"]
mod initial_cursor_tests;

/// 页链与合并：同一 cursor 的等价页、limit 只改页界、追加与再次 Full 后成员不变。
#[path = "history_page_chain_test.rs"]
mod chain_tests;

/// 拒绝路径：分页准入（能力门 / 参数 / 会话身份）与「拒绝不降级成初始 load」。
#[path = "history_page_rejection_test.rs"]
mod rejection_tests;

/// rewind 与过期游标：结构版本前进后已发出的游标失效，且不降级成初始 load。
#[path = "history_page_rewind_test.rs"]
mod rewind_tests;

/// 能力门控与私有字段：连接级协商（initialize 请求 / 直接写入）决定回放与分页语义。
/// 模块名参与 `cargo test` 过滤（`history_page_caps`）。
#[path = "history_page_caps_test.rs"]
mod history_page_caps;

/// `peri.historyWindowV1`：open/page/seek、二次校验、fixed tail、descriptor 与分块。
#[path = "history_page_window_test.rs"]
mod window_tests;

/// History V2 own 适配器：真实 SQLite 上的 open/page/seek/body、fixed tail 与 opaque。
/// 模块名参与 `cargo test` 过滤（`history_v2_adapter`）。
#[path = "history_v2_adapter_test.rs"]
mod history_v2_adapter_tests;

/// History V2 inherited 冻结前缀：跨区 open/page/seek、ordinal、body 分块与可见性。
/// 模块名参与 `cargo test` 过滤（`history_v2_adapter`）。
#[path = "history_v2_adapter_inherited_test.rs"]
mod history_v2_adapter_inherited_tests;

/// History V2 `session/load` initial（H4b）：协商、首屏、幂等重放、失效与失败 effect。
/// 模块名参与 `cargo test` 过滤（`history_v2_load`）。
#[path = "history_v2_load_test.rs"]
mod history_v2_load_tests;

/// H8：History V2 wire 端到端（真实 initialize + initial + page/seek/body + live + close）。
/// 模块名参与 `cargo test` 过滤（`history_v2_end_to_end`）。
#[path = "history_v2_end_to_end_test.rs"]
mod history_v2_end_to_end_tests;

/// G5：session/load 不在全局 sessions 锁内做资源/DB I/O。
#[path = "history_page_window_lock_test.rs"]
mod window_lock_tests;

/// 真实 SQLite 门面 + 已注册会话（`register_session_with_history`：3 条 own 条目）。
///
/// `cfg` 是 `Arc`：分页用例既要把配置交给 `ServerLoop`（它按请求逐个派发），又要在运行期
/// 继续用同一份门面制造追加 / 再次 Full（REQ-H02 的边界用例）。
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

    /// Full Compact 的存储侧形状：把最旧 `count` 条 own 条目标成 excluded 并追加摘要。
    async fn archive(&self, count: usize) -> Vec<MessageId> {
        let ids: Vec<MessageId> = self.own_ids().await.into_iter().take(count).collect();
        self.cfg
            .session_resources
            .apply_compaction(
                &self.sid,
                &CompactionChange {
                    flag_updates: ids
                        .iter()
                        .map(|id| {
                            (
                                *id,
                                MessageFlags {
                                    excluded: true,
                                    ..Default::default()
                                },
                            )
                        })
                        .collect(),
                    appended_messages: vec![BaseMessage::ai("compacted summary")],
                },
            )
            .await
            .unwrap();
        ids
    }

    /// 追加实时尾部消息（REQ-H02：追加不得使在途游标失效，也不得挤动旧页内容）。
    async fn append(&self, count: usize) {
        let payloads: Vec<PersistedPayload> = (0..count)
            .map(|index| PersistedPayload::Message(BaseMessage::human(format!("尾部追加 {index}"))))
            .collect();
        self.cfg
            .session_resources
            .append_history(&self.sid, &payloads)
            .await
            .unwrap();
    }

    /// 初始 load 的分页身份（`dispatch_request` 响应后置步骤的直接调用）。
    async fn initial_cursor(&self) -> Value {
        let mut response = json!({ "sessionId": self.sid });
        history_page::attach_initial_cursor(&self.cfg, &self.sessions, &self.sid, &mut response)
            .await
            .unwrap();
        response
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

/// 用**指定**的服务端 transport 起 `ServerLoop`：用例要控制出站通道（例如让通知失败）时用。
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

fn pagination_caps() -> PeriCaps {
    PeriCaps {
        history_pagination: true,
        replay: true,
        ..Default::default()
    }
}

/// 拒绝的响应面：错误码 + `data.reason` 精确匹配，且 `data` **只有** `reason` 一个键。
///
/// 初始 load 的响应字段（`nextCursor` / `snapshotToken` / `modes` / `configOptions` /
/// `_meta.periLoadGeneration` / `_meta.peri.sessionWorkspaceV1`）一个都不许随拒绝出现：
/// 它们同时是「这次请求被降级成初始 load」的指纹。
fn assert_page_rejection(
    error: &crate::transport::types::AcpError,
    code: i64,
    reason: &str,
    label: &str,
) {
    assert_eq!(error.code, code, "{label}：错误码");
    let data = error
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("{label}：拒绝必须带 data.reason，实际：{error}"));
    assert_eq!(
        data.get("reason").and_then(Value::as_str),
        Some(reason),
        "{label}：data.reason 必须精确匹配（客户端按它分类），实际 data：{data}"
    );
    assert_eq!(
        data.as_object()
            .expect("data 必须是对象")
            .keys()
            .collect::<Vec<_>>(),
        vec!["reason"],
        "{label}：拒绝的 data 只允许 reason，初始 load 的字段不得出现：{data}"
    );
}

/// 会话热态的四元事实（拒绝前后各取一次）：有效视图条数、legacy 投影条数、结构版本、
/// 执行环境是否仍为「未装配」。初始 load 会装配环境并改写前两项，分页拒绝三项都不许动。
async fn session_facts(
    sessions: &crate::host::SharedSessions,
    sid: &str,
) -> (usize, usize, u64, bool) {
    let states = sessions.lock().await;
    let state = states.get(sid).expect("会话必须仍在集合里");
    (
        state.effective_messages().len(),
        state.history_payloads.len(),
        state.structure_version,
        state.environment.is_none(),
    )
}

/// 一次浏览的 wire 形状：逐页向前翻到 `nextCursor = null`，返回逐页的条目（页内历史正序）。
///
/// 保留「页」这一层：页链方向（每一页严格更早于上一页）与页长本身就是契约的一部分，
/// 拍平会把它丢掉。
async fn walk_pages(
    client: &crate::transport::mpsc::MpscClientTransport,
    session_id: &str,
    cursor: &Value,
    limit: u64,
) -> Vec<Vec<Value>> {
    let mut pages = Vec::new();
    let mut cursor = cursor.clone();
    for _ in 0..16 {
        let reply = client
            .send_request(
                "session/load",
                json!({ "sessionId": session_id, "cursor": cursor, "limit": limit }),
            )
            .await
            .unwrap_or_else(|error| panic!("翻页必须成功（limit={limit}）：{error:?}"));
        pages.push(reply["items"].as_array().expect("页必须带 items").clone());
        match reply["nextCursor"].as_str() {
            Some(next) => cursor = Value::String(next.to_owned()),
            None => return pages,
        }
    }
    panic!("分页链没有收敛");
}

/// 页链的契约（返回逐页的位置）：页内按 `position` 升序（历史正序），每一页严格更早于上一页。
fn assert_page_chain(pages: &[Vec<Value>]) -> Vec<Vec<u64>> {
    let per_page: Vec<Vec<u64>> = pages
        .iter()
        .map(|page| {
            page.iter()
                .map(|item| item["position"].as_u64().expect("条目必须带 position"))
                .collect()
        })
        .collect();
    for page in &per_page {
        assert!(
            page.windows(2).all(|pair| pair[0] < pair[1]),
            "页内按历史正序（position 递增）：{page:?}"
        );
    }
    for pair in per_page.windows(2) {
        assert!(
            pair[1].iter().max() < pair[0].iter().min(),
            "每一页严格更早于上一页：{pair:?}"
        );
    }
    per_page
}

/// 客户端按 `position` 合并后的逻辑序列（历史正序）：它才是「遍历顺序」的可比较形式，
/// 页只是把这段序列切成若干段（切法由 limit 决定）。
fn logical_sequence(pages: &[Vec<Value>]) -> Vec<u64> {
    let mut positions: Vec<u64> = pages
        .iter()
        .flatten()
        .map(|item| item["position"].as_u64().expect("条目必须带 position"))
        .collect();
    positions.sort_unstable();
    positions
}

/// 非阻塞取走客户端队列里**已经入队**的全部通知。
///
/// 服务端把通知与响应写进同一个无界通道、由同一个 pump 顺序转发，因此 `session/load` 的响应
/// 被 `RequestRouter` 分流时，它在响应**之前**发出的回放通知已全部进入客户端队列。这里用
/// `biased` + `ready(())` 只做「取到空为止」，既不会漏也不会等：不依赖任何等待时限。
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
                // 非通知消息（响应经 router 分流，不走这条队列）与通道关闭都表示队列已空。
                Some(_) => {}
                None => break,
            },
            _ = std::future::ready(()) => break,
        }
    }
    out
}
