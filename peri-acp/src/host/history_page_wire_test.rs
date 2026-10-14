//! 历史页与初始回放的 wire 级行为（REQ §5.1~§5.3、验收 A08/A09/A15/A20/A21/A22/A25）。
//!
//! 走真实的 `ServerLoop` + MPSC transport：模式分流、执行锁与响应装配是
//! `dispatch_request` 的职责，只测下层函数会漏掉「分页是否被执行锁挡住」。

use super::*;
use crate::host::history_page;
use crate::transport::AcpTransport;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::store::{CompactionChange, MessageFlags};
use peri_acp_types::PeriCaps;
use tokio_util::sync::CancellationToken;

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

/// A21：翻页与正在执行的 turn 并行——分页不取执行生命周期锁，也不回答 -32010。
#[tokio::test]
#[serial]
async fn a_page_request_is_answered_while_the_session_holds_the_execution_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let archived = fixture.archive(2).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        spawn_loop(Arc::clone(&cfg), fixture.sessions, Some(pagination_caps())).await;
    let sid = fixture.sid.clone();
    // 执行中的 turn：该会话的 prompt 锁被持有。
    let prompt_lock = Arc::new(tokio::sync::Mutex::new(()));
    let held = Arc::clone(&prompt_lock).lock_owned().await;
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().cancel_token = Some(CancellationToken::new());
    }

    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 50 }),
        ),
    )
    .await
    .expect("分页不得排队等待执行生命周期锁")
    .expect("分页请求必须成功");

    let items = reply["items"].as_array().expect("历史页必须带 items");
    assert_eq!(
        items.len(),
        archived.len(),
        "归档正文正是分页要读的内容：{reply}"
    );
    assert!(reply.get("nextCursor").is_some(), "页响应必须携带游标字段");
    // 不装配、不改绑定：会话状态里没有 environment，也没有被广播的配置更新。
    let states = sessions.lock().await;
    let state = states.get(&sid).expect("分页不改动会话集合");
    assert!(
        state.environment.is_none(),
        "分页不得执行任务绑定 / MCP attach"
    );
    assert!(!state.closing);
    drop(states);
    drop(held);
    task.abort();
}

/// A25 / REQ §5.3：未协商分页能力的连接带分页字段 → 明确拒绝，且不降级为初始 load。
#[tokio::test]
#[serial]
async fn pagination_fields_from_an_unnegotiated_connection_are_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let archived = fixture.archive(1).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("游标由能力协商前的读取给出")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(
        Arc::clone(&cfg),
        fixture.sessions,
        Some(PeriCaps::default()),
    )
    .await;
    let sid = fixture.sid.clone();
    let before = sessions.lock().await.get(&sid).unwrap().history.len();

    let error = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor }),
        )
        .await
        .expect_err("未协商能力时携带 cursor 必须拒绝");
    assert_eq!(error.code, -32601, "能力门控与既有 rewind/oauth 同码");
    assert_eq!(
        sessions.lock().await.get(&sid).unwrap().history.len(),
        before,
        "拒绝不得触发一次初始 load"
    );
    assert_eq!(archived.len(), 1);
    task.abort();
}

/// A15 / REQ-H01-1：初始游标锚在自有有效区最旧一条，绝不退回首屏第一条。
#[tokio::test]
#[serial]
async fn initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;

    // 普通新会话：没有更早范围 → nextCursor = null（不编造游标）。
    let fresh = fixture.initial_cursor().await;
    assert_eq!(fresh["nextCursor"], Value::Null);
    assert!(fresh["snapshotToken"].is_string(), "浏览身份必须随响应给出");

    // 归档后：锚点 = 最旧的自有**有效**条目（摘要），不是首屏第一条（更不是被排除的原文）。
    let archived = fixture.archive(2).await;
    let response = fixture.initial_cursor().await;
    let cursor = response["nextCursor"]
        .as_str()
        .expect("归档历史存在时必须给出游标")
        .to_owned();
    let cursor = peri_acp_types::history_page::HistoryCursorEnvelope::decode(&cursor, &fixture.sid)
        .expect("游标必须可解码");
    assert_eq!(
        response["snapshotToken"].as_str(),
        Some(cursor.snapshot_token.as_str()),
        "游标与响应携带同一份浏览身份"
    );
    assert!(
        cursor.snapshot_token.ends_with(&cursor.cursor.range_token),
        "浏览身份含范围身份（另一半是存储看不见的投影口径）：{}",
        cursor.snapshot_token
    );

    let page = crate::dispatch::load_history_page(
        fixture.cfg.controller.as_ref(),
        &fixture.sid,
        &cursor.cursor,
        50,
        history_page::PAGE_BYTE_BUDGET,
    )
    .await
    .expect("首游标必须可用");
    let ids: Vec<MessageId> = page.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, archived, "首屏之后的页按历史正序补齐归档区");
    assert!(
        !ids.contains(&cursor.cursor.anchor),
        "页与首屏不得重叠：锚点条目已在首屏显示"
    );
    assert!(
        page.next_cursor.is_none(),
        "读到范围起点即收尾（更早方向没有内容）"
    );
}

/// 只让**通知出口**失败的 transport：注入一次回放投递失败（验收 A22 的服务端半边）。
///
/// 请求 / 响应通道原样委托给真实 MPSC 服务端：失败点必须精确落在回放的那条通知上，
/// 否则「load 失败」可能来自别的通道故障，用例就证明不了回放与 load 成败的绑定关系。
struct NotificationFailingTransport {
    inner: crate::transport::mpsc::MpscServerTransport,
}

#[async_trait::async_trait]
impl AcpTransport for NotificationFailingTransport {
    async fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, crate::transport::types::AcpError> {
        self.inner.send_request(method, params).await
    }

    async fn send_notification(
        &self,
        _method: &str,
        _params: Value,
    ) -> Result<(), crate::transport::types::AcpError> {
        Err(crate::transport::types::AcpError::new(
            -32603,
            "replay delivery failed (injected)",
        ))
    }

    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        self.inner.recv().await
    }

    async fn send_response(
        &self,
        id: crate::transport::types::RequestId,
        result: Result<Value, crate::transport::types::AcpError>,
    ) -> Result<(), crate::transport::types::AcpError> {
        self.inner.send_response(id, result).await
    }
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

/// A08：同一 cursor、同一生效 `limit` 重试 3 次给出等价页；换 `limit` 只改页长与边界，
/// 逻辑遍历顺序不变。
#[tokio::test]
#[serial]
async fn the_same_cursor_replays_equivalent_pages_and_limit_only_changes_page_boundaries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    // 归档最旧 4 条（并追加一条摘要）：首屏之后有 4 条更早历史，足以分两页。
    fixture.archive(4).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(cfg, fixture.sessions.clone(), Some(pagination_caps())).await;
    let sid = fixture.sid.clone();
    let cursor = Value::String(cursor);

    let first = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 2 }),
        )
        .await
        .expect("首游标必须可用");
    assert_eq!(
        first["items"].as_array().unwrap().len(),
        2,
        "limit=2 决定页长"
    );

    // 相同 cursor + 相同生效 limit 重试两次：条目身份、内容、顺序与后续位置全部等价。
    for attempt in 2..=3 {
        let again = client
            .send_request(
                "session/load",
                json!({ "sessionId": sid, "cursor": cursor, "limit": 2 }),
            )
            .await
            .unwrap_or_else(|error| panic!("第 {attempt} 次重试必须成功：{error:?}"));
        assert_eq!(
            again["items"], first["items"],
            "第 {attempt} 次重试的页成员必须等价"
        );
        assert_eq!(
            again["nextCursor"], first["nextCursor"],
            "第 {attempt} 次重试的后续位置必须等价"
        );
        assert_eq!(again["snapshotToken"], first["snapshotToken"]);
    }

    // 换 limit：页长与页边界随之改变，但逻辑遍历顺序不变。
    let shorter = walk_pages(&client, &sid, &cursor, 2).await;
    let longer = walk_pages(&client, &sid, &cursor, 3).await;
    assert_page_chain(&shorter);
    assert_page_chain(&longer);
    assert_eq!(shorter[0].len(), 2, "limit=2 的首页两条");
    assert_eq!(longer[0].len(), 3, "limit 改变页长");

    // 同一段历史、同一个逻辑顺序：覆盖到的条目身份集合与 position 序列都一致，
    // 差别只在「哪几条凑成同一页」。
    let entry_ids = |pages: &[Vec<Value>]| {
        let mut ids: Vec<String> = pages
            .iter()
            .flatten()
            .map(|item| item["entryId"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(
        entry_ids(&shorter),
        entry_ids(&longer),
        "limit 不改变覆盖范围"
    );
    assert_eq!(
        logical_sequence(&shorter),
        logical_sequence(&longer),
        "limit 只改页边界：合并后的逻辑顺序不变"
    );
    assert_eq!(
        logical_sequence(&shorter).len(),
        4,
        "归档区的全部更早条目都要被翻出来"
    );
    task.abort();
}

/// A09：追加消息并再次 Full 之后，旧 cursor 与旧 `nextCursor` 仍然可用，页成员不被新的
/// `excluded` 或新的有效视图重新筛选。
#[tokio::test]
#[serial]
async fn a_cursor_survives_appends_and_another_full_without_moving_its_page_members() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    fixture.archive(4).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(cfg, fixture.sessions.clone(), Some(pagination_caps())).await;
    let sid = fixture.sid.clone();
    let cursor = Value::String(cursor);

    let first = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor.clone(), "limit": 2 }),
        )
        .await
        .expect("首页必须成功");
    let next = first["nextCursor"].clone();
    let second = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": next.clone(), "limit": 2 }),
        )
        .await
        .expect("第二页必须成功");

    // 实时尾部更新 + 再次 Full：追加只动尾部，Full 只改 flags 并追加摘要。
    fixture.append(2).await;
    fixture.archive(1).await;

    let first_again = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": cursor, "limit": 2 }),
        )
        .await
        .expect("旧 cursor 不得被追加与 Full 弄失效");
    assert_eq!(
        first_again["items"], first["items"],
        "旧页身份、内容与顺序必须等价"
    );
    assert_eq!(
        first_again["nextCursor"], first["nextCursor"],
        "旧页的后续位置必须等价"
    );

    let second_again = client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "cursor": next, "limit": 2 }),
        )
        .await
        .expect("在途的旧 nextCursor 也必须仍然可用");
    assert_eq!(
        second_again["items"], second["items"],
        "下一段成员不得被重筛"
    );
    assert_eq!(second_again["nextCursor"], second["nextCursor"]);
    task.abort();
}

/// A22（服务端半边）：某一条初始回放投递失败 → 本次 load 以错误结束，且**不给出**任何
/// 分页身份（失败代次没有成功 cursor）。
///
/// 控制组先证明「同一份归档状态下，生产的初始游标函数本来会给出游标」：因此这次失败确实
/// 是回放投递造成的，而不是这条会话本来就没有更早历史。
#[tokio::test]
#[serial]
async fn a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    fixture.archive(4).await;
    assert!(
        fixture.initial_cursor().await["nextCursor"].is_string(),
        "控制组：这条会话本来会给出游标"
    );

    let cfg = Arc::clone(&fixture.cfg);
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let failing: Arc<dyn AcpTransport> = Arc::new(NotificationFailingTransport { inner: server });
    let (client, sessions, task) = spawn_loop_with_transport(
        cfg,
        fixture.sessions.clone(),
        Some(pagination_caps()),
        client,
        failing,
    )
    .await;
    let sid = fixture.sid.clone();

    let error = client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect_err("回放投递失败必须让初始 load 失败");
    assert_eq!(error.code, -32603, "回放失败按内部错误上报：{error:?}");
    assert!(
        error.message.contains("history replay failed"),
        "错误必须指明是回放失败：{error:?}"
    );
    // 失败响应里不可能有分页身份，会话也不留半加载态（下一次 load 仍可从同一份历史重试）。
    let states = sessions.lock().await;
    let state = states
        .get(&sid)
        .expect("失败的 load 不得删除已发布的会话状态");
    assert!(!state.closing, "回放失败不是会话关闭");
    drop(states);
    task.abort();
}

/// 合并契约（wire 级，REQ-H01-3 / A20）：协商分页的连接做初始 load 时，首屏每一条有效条目
/// 都带上与历史页**同一把尺子**的身份与位置，页覆盖的只是更早的那一段——客户端才能把稀疏
/// 首屏与后续页拼成一段不间断的历史。
///
/// 刻意走冷恢复（loop 手里的会话集合是空的）：只有这条路径的首屏与位置表同出于存储的有效
/// 视图。`AlreadyLive` 会沿用 fixture 内存里那份尚未被 compaction 改写的 `history_payloads`，
/// 那样测到的只是 fixture 自己。
#[tokio::test]
#[serial]
async fn a_negotiated_initial_load_marks_the_first_screen_with_page_comparable_identities() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.append(3).await;
    let archived = fixture.archive(4).await;
    let cursor = fixture.initial_cursor().await["nextCursor"]
        .as_str()
        .expect("归档历史存在时初始 load 必须给出游标")
        .to_owned();
    let own_ids = fixture.own_ids().await;
    let expected_first_screen = own_ids.len() - archived.len();
    assert_eq!(
        (expected_first_screen, archived.len()),
        (3, 4),
        "fixture 前提：有效视图 3 条 + 归档区 4 条"
    );

    let cfg = Arc::clone(&fixture.cfg);
    let cold: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (client, _sessions, task) = spawn_loop(cfg, cold, Some(pagination_caps())).await;
    let sid = fixture.sid.clone();

    let load = {
        let client = Arc::clone(&client);
        let sid = sid.clone();
        tokio::spawn(async move {
            client
                .send_request("session/load", json!({ "sessionId": sid }))
                .await
        })
    };

    // 回放必须在响应**之前**发完：先收满首屏的条目身份，再取响应。少一条就说明首屏里有条目
    // 没被标记，客户端也就无从把它并进页序列。
    let mut marks: Vec<(String, u64)> = Vec::new();
    let mut generations: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while marks.len() < expected_first_screen {
        let incoming = tokio::time::timeout_at(deadline, client.recv())
            .await
            .expect("首屏条目必须在时限内全部带上身份与位置")
            .expect("回放结束之前客户端通道不得关闭");
        let crate::transport::types::IncomingMessage::Notification { method, params } = incoming
        else {
            continue;
        };
        if method != "session/update" {
            continue;
        }
        let meta = &params["update"]["_meta"];
        let Some(entry) = meta["periEntryId"].as_str() else {
            continue;
        };
        if marks.iter().any(|(seen, _)| seen == entry) {
            continue;
        }
        marks.push((
            entry.to_owned(),
            meta["periPosition"]
                .as_u64()
                .unwrap_or_else(|| panic!("首屏自有条目必须带存储序位置：{meta}")),
        ));
        generations.push(
            meta["periLoadGeneration"]
                .as_str()
                .unwrap_or_else(|| panic!("位置戳与本次 load 的代次同进同出：{meta}"))
                .to_owned(),
        );
    }
    let reply = load
        .await
        .expect("load 任务不得 panic")
        .expect("初始 load 必须成功");

    // 首屏覆盖自有条目里**没被归档**的那一段，且逐条带身份。
    let mut first_screen: Vec<String> = marks.iter().map(|(entry, _)| entry.clone()).collect();
    first_screen.sort();
    let mut expected: Vec<String> = own_ids
        .iter()
        .filter(|id| !archived.contains(id))
        .map(|id| crate::dispatch::entry_id(&sid, *id))
        .collect();
    expected.sort();
    assert_eq!(first_screen, expected, "首屏条目必须逐条带身份");
    assert!(
        generations.iter().all(|seen| seen == &generations[0]),
        "一次 load 只有一个代次：{generations:?}"
    );
    assert_eq!(
        Some(generations[0].as_str()),
        reply["_meta"]["periLoadGeneration"].as_str(),
        "回放通知与成功响应必须带同一个代次（REQ-H04）"
    );

    // 更早的那一段：页给出的身份与归档段逐条对应，且位置严格小于首屏的最低位置——两段不重叠，
    // 合起来正好是全部自有条目。
    let pages = walk_pages(&client, &sid, &Value::String(cursor), 2).await;
    assert_page_chain(&pages);
    let mut page_entries: Vec<String> = pages
        .iter()
        .flatten()
        .map(|item| {
            item["entryId"]
                .as_str()
                .expect("页条目必须带身份")
                .to_owned()
        })
        .collect();
    page_entries.sort();
    let mut archived_entries: Vec<String> = archived
        .iter()
        .map(|id| crate::dispatch::entry_id(&sid, *id))
        .collect();
    archived_entries.sort();
    assert_eq!(page_entries, archived_entries, "页覆盖的就是归档的那一段");
    let page_positions = logical_sequence(&pages);
    let lowest_first_screen = marks
        .iter()
        .map(|(_, position)| *position)
        .min()
        .expect("首屏必须至少有一条带位置的条目");
    assert!(
        page_positions
            .iter()
            .all(|position| *position < lowest_first_screen),
        "页必须严格早于首屏：首屏最低位置 {lowest_first_screen}，页 {page_positions:?}"
    );
    assert_eq!(
        page_positions.len() + marks.len(),
        own_ids.len(),
        "首屏 + 页必须覆盖全部自有条目"
    );
    task.abort();
}

// ── 独立复核（S9-recheck：未协商连接的完整回放与私有语义边界）──────────────────

/// JSON 树里出现过的全部键（递归，含 `_meta` 内层）。
fn s9_recheck_all_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                out.push(key.clone());
                s9_recheck_all_keys(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| s9_recheck_all_keys(item, out)),
        _ => {}
    }
}

/// 跑一次未协商分页的**真实 ServerLoop** `session/load`：返回（回放文本序列，通知 + 响应
/// 里出现过的全部键，响应体）。
///
/// 会话级 caps 注册表的值由调用方决定（模拟「这条会话此前被谁写过」）；连接级事实一律是
/// 标准客户端（`initialize` 声明里没有任何 peri 扩展）。
async fn s9_recheck_unnegotiated_load(
    fixture: &PageFixture,
    session_registry_caps: PeriCaps,
) -> (Vec<String>, Vec<String>, Value) {
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), session_registry_caps);

    let canonical_len = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .len();

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(
        Arc::clone(&cfg),
        Arc::clone(&fixture.sessions),
        Some(PeriCaps::default()),
    )
    .await;
    let sid = fixture.sid.clone();

    let for_load = Arc::clone(&client);
    let load = tokio::spawn(async move {
        for_load
            .send_request("session/load", json!({ "sessionId": sid }))
            .await
    });

    let mut texts: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while texts.len() < canonical_len {
        let incoming = tokio::time::timeout_at(deadline, client.recv())
            .await
            .expect("回放必须在时限内发完")
            .expect("客户端通道在响应之前不得关闭");
        let crate::transport::types::IncomingMessage::Notification { method, params } = incoming
        else {
            continue;
        };
        s9_recheck_all_keys(&params, &mut keys);
        if method != "session/update" {
            continue;
        }
        if let (Some(kind), Some(text)) = (
            params["update"]["sessionUpdate"].as_str(),
            params["update"]["content"]["text"].as_str(),
        ) {
            if matches!(kind, "user_message_chunk" | "agent_message_chunk") {
                texts.push(text.to_owned());
            }
        }
    }
    let reply = load
        .await
        .expect("load 任务不得 panic")
        .expect("标准客户端的 session/load 必须成功");
    s9_recheck_all_keys(&reply, &mut keys);
    task.abort();
    (texts, keys, reply)
}

/// 未协商分页的连接（标准 ACP 客户端）经真实 `ServerLoop` 的 `session/load`：
/// 必须交付完整 canonical 历史（含 Full 归档原文），且响应与通知里不得出现分页私有字段。
#[tokio::test]
#[serial]
async fn s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let archived = fixture.archive(2).await;
    assert_eq!(archived.len(), 2, "夹具前提：最旧两条 own 条目被 Full 归档");

    let canonical: Vec<String> = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .iter()
        .filter_map(|payload| payload.as_message().map(|message| message.content()))
        .collect();
    assert!(
        canonical.iter().any(|text| text == "第一轮用户问题"),
        "夹具前提：canonical 读取含归档原文: {canonical:?}"
    );

    let (texts, keys, reply) = s9_recheck_unnegotiated_load(&fixture, PeriCaps::default()).await;

    assert_eq!(
        texts, canonical,
        "未协商连接必须收到完整 canonical 回放（含归档原文），顺序一致"
    );
    assert!(
        reply.get("nextCursor").is_none() && reply.get("snapshotToken").is_none(),
        "未协商连接的响应不得携带分页字段: {reply}"
    );
    for forbidden in ["nextCursor", "snapshotToken", "periEntryId", "periPosition"] {
        assert!(
            !keys.iter().any(|key| key == forbidden),
            "分页私有字段 {forbidden} 不得出现在未协商连接的 wire 上"
        );
    }
}

/// REQ §5.3 的反例回归：**会话级 caps 注册表**已被先前的 peri 连接写过（`replay` 与
/// `history_pagination` 都是 true）时，当前这个未协商任何 peri 扩展的标准连接**不得**收到任何
/// Peri 私有字段——能力以**当前连接**的协商结果为准，不继承会话内保存的旧值（验收 A25）。
///
/// 本用例原为 S9 独立复核留下的 `#[ignore]` 反例探针（`.recon/S9-verification.md` 第 8 节：
/// 当时 `periReplay` 4 条通知 + `periLoadGeneration` 响应与通知都泄漏）；S9c 把回放标记的门
/// 改成连接事实后转为回归用例。
#[tokio::test]
#[serial]
async fn s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;

    let (_texts, keys, _reply) = s9_recheck_unnegotiated_load(&fixture, pagination_caps()).await;

    let leaked: Vec<String> = [
        "nextCursor",
        "snapshotToken",
        "periEntryId",
        "periPosition",
        "periLoadGeneration",
        "periReplay",
    ]
    .into_iter()
    .filter_map(|forbidden| {
        let count = keys.iter().filter(|key| *key == forbidden).count();
        (count > 0).then(|| format!("{forbidden} x{count}"))
    })
    .collect();
    assert!(
        leaked.is_empty(),
        "未协商分页的连接不得继承会话级 caps 带来的私有字段；实际命中: {leaked:?}"
    );
}

/// 跑一次**协商过 peri 能力**的**真实 ServerLoop** `session/load`：返回（回放通知上出现过的
/// 代次，通知 + 响应里出现过的全部键，响应体）。
///
/// 与 [`s9_recheck_unnegotiated_load`] 只差连接声明。通知读取用 `biased` 的 `select!`：只要
/// 通道里还有通知就先取通知，取完才观察 load 完成——回放通知总在响应之前送出，因此不会漏。
async fn s9_recheck_negotiated_load(
    fixture: &PageFixture,
    session_registry_caps: PeriCaps,
    connection_caps: PeriCaps,
) -> (Vec<String>, Vec<String>, Value) {
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), session_registry_caps);

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(
        Arc::clone(&cfg),
        Arc::clone(&fixture.sessions),
        Some(connection_caps),
    )
    .await;
    let sid = fixture.sid.clone();

    let for_load = Arc::clone(&client);
    let mut load = Box::pin(tokio::spawn(async move {
        for_load
            .send_request("session/load", json!({ "sessionId": sid }))
            .await
    }));

    let mut keys: Vec<String> = Vec::new();
    let mut generations: Vec<String> = Vec::new();
    let reply = loop {
        tokio::select! {
            biased;
            incoming = client.recv() => {
                match incoming {
                    Some(crate::transport::types::IncomingMessage::Notification { params, .. }) => {
                        s9_recheck_all_keys(&params, &mut keys);
                        if let Some(generation) =
                            params["update"]["_meta"]["periLoadGeneration"].as_str()
                        {
                            generations.push(generation.to_owned());
                        }
                    }
                    // 非通知消息（响应不经这条通道）与通道关闭都只说明「回放通知不会再有」：
                    // 等 load 自身的结果，不做任何 busy loop。
                    Some(_) => {}
                    None => break (&mut load).await.expect("load 任务不得 panic"),
                }
            }
            done = &mut load => break done.expect("load 任务不得 panic"),
        }
    };
    let reply = reply.expect("协商连接的 session/load 必须成功");
    s9_recheck_all_keys(&reply, &mut keys);
    task.abort();
    (generations, keys, reply)
}

/// 门控没被改死：同一场景下**当前连接**协商了 peri 能力的客户端**仍然**收到 `periReplay` 与
/// `periLoadGeneration`（响应与回放通知同一个代次）。
///
/// 会话级注册表刻意写 `PeriCaps::default()`：门若回读注册表（无论继承旧值还是被旧值压住），
/// 本用例的标记断言都会失败。
#[tokio::test]
#[serial]
async fn s9_recheck_a_negotiated_connection_still_receives_the_replay_marks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;

    let (generations, keys, reply) =
        s9_recheck_negotiated_load(&fixture, PeriCaps::default(), pagination_caps()).await;

    assert!(
        keys.iter().any(|key| key == "periReplay"),
        "协商过回放能力的连接仍必须收到 periReplay: {keys:?}"
    );
    assert!(
        keys.iter().any(|key| key == "periLoadGeneration"),
        "协商过回放能力的连接仍必须收到 periLoadGeneration: {keys:?}"
    );
    assert!(
        !generations.is_empty(),
        "回放通知上必须带代次（不只是响应）: {keys:?}"
    );
    assert!(
        generations
            .iter()
            .all(|seen| Some(seen.as_str()) == reply["_meta"]["periLoadGeneration"].as_str()),
        "回放通知与成功响应必须带同一个代次（REQ-H04）：{generations:?} 对 {reply}"
    );
}

// ── 独立复核（S9c-verification：`initialize` **请求**到连接事实的接线）────────────
//
// 上段三条 `s9_recheck_*` 都把连接 caps 直接 `commit_initialize` 进 `ConnectionContext`，绕过
// `initialize` 请求本身；以下三条走**真实请求**（`initialize` → `handle_initialize` →
// `commit_initialize`）驱动同一份连接事实，覆盖 §5.3 三类连接（标准客户端 / peri 客户端 /
// 从未 `initialize` 的进程内路径）。夹具、`ServerLoop`、SQLite 门面与上段同源。

/// 六个 Peri 私有回放 / 分页字段（复核判据的完整口径）。
const S9C_PRIVATE_FIELDS: [&str; 6] = [
    "periReplay",
    "periLoadGeneration",
    "nextCursor",
    "snapshotToken",
    "periEntryId",
    "periPosition",
];

/// 标准 ACP 客户端的 `initialize` 声明：带客户端能力字段，但**没有任何** peri 扩展。
fn s9c_standard_initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": false } },
    })
}

/// peri 客户端的 `initialize` 声明：回放标记 + 历史分页（TUI / SDK 的形状）。
fn s9c_peri_initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "_meta": { "peri.replay": true, "peri.historyPagination": true },
        },
    })
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

/// 一次 load 的 wire 事实：回放正文序列、出现过的全部键、逐条通知的 `_meta`。
async fn s9c_wire_facts(
    client: &crate::transport::mpsc::MpscClientTransport,
    reply: &Value,
) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let notifications = s9c_drain_notifications(client).await;
    assert!(
        !notifications.is_empty(),
        "load 必须发出通知（否则本用例是空转）"
    );
    let mut keys: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut metas: Vec<Value> = Vec::new();
    for (method, params) in &notifications {
        s9_recheck_all_keys(params, &mut keys);
        if method != "session/update" {
            continue;
        }
        let update = &params["update"];
        if let (Some(kind), Some(text)) = (
            update["sessionUpdate"].as_str(),
            update["content"]["text"].as_str(),
        ) {
            if matches!(kind, "user_message_chunk" | "agent_message_chunk") {
                texts.push(text.to_owned());
            }
        }
        if let Some(meta) = update.get("_meta") {
            metas.push(meta.clone());
        }
    }
    s9_recheck_all_keys(reply, &mut keys);
    (texts, keys, metas)
}

/// 命中的私有字段（`字段 x次数`），空 = 一个都没出现。
fn s9c_hits(keys: &[String]) -> Vec<String> {
    S9C_PRIVATE_FIELDS
        .into_iter()
        .filter_map(|field| {
            let count = keys.iter().filter(|key| *key == field).count();
            (count > 0).then(|| format!("{field} x{count}"))
        })
        .collect()
}

/// 用例 A（复核者新增）：连接**经真实 `initialize` 请求**声明为标准客户端（声明里没有 peri
/// 扩展），会话级注册表已被先前的 peri 连接写满（`replay` + `historyPagination`）。
///
/// 断言：响应与全部通知**一个私有字段都没有**，且交付正文仍是完整 canonical（含 Full 归档
/// 原文）。与上段 `s9_recheck_unnegotiated_load` 的差别是**接线面**：那三条把 caps 直接写进
/// `ConnectionContext`，本用例驱动 `initialize` 请求 → `initialize_peri_caps` →
/// `commit_initialize` 这条真实链路；接线若断（解析位置错、未落连接），只有本用例会红。
#[tokio::test]
#[serial]
async fn s9c_verification_an_initialize_without_peri_extensions_yields_no_private_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let canonical: Vec<String> = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .iter()
        .filter_map(|payload| payload.as_message().map(|message| message.content()))
        .collect();
    assert!(
        canonical.iter().any(|text| text == "第一轮用户问题"),
        "夹具前提：canonical 含被 Full 归档的原文: {canonical:?}"
    );

    // 这条会话此前被 peri 连接注册过：注册表里存着旧客户端的全能力值。
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), pagination_caps());

    let cfg = Arc::clone(&fixture.cfg);
    // 连接侧**不预置**任何协商值：门必须由下面这条真实 `initialize` 请求建立。
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), Arc::clone(&fixture.sessions), None).await;

    client
        .send_request("initialize", s9c_standard_initialize_params())
        .await
        .expect("标准客户端的 initialize 必须成功");
    let reply = client
        .send_request("session/load", json!({ "sessionId": fixture.sid }))
        .await
        .expect("未协商 peri 扩展的连接，session/load 必须成功");
    let (texts, keys, _metas) = s9c_wire_facts(&client, &reply).await;
    task.abort();

    assert_eq!(
        s9c_hits(&keys),
        Vec::<String>::new(),
        "initialize 未声明 peri 扩展的连接不得继承会话级注册表的旧能力: {keys:?}"
    );
    assert_eq!(
        texts, canonical,
        "未协商连接必须收到完整 canonical 回放（含归档原文）"
    );
}

/// 用例 B（复核者新增）：连接**经真实 `initialize` 请求**声明了 `peri.replay` +
/// `peri.historyPagination`，而会话级注册表**刻意**留着 `PeriCaps::default()`。
///
/// 断言：回放标记与分页字段**照旧出现**（门没被改成「永远关闭」），且通知与响应同一个代次。
#[tokio::test]
#[serial]
async fn s9c_verification_an_initialize_with_peri_extensions_keeps_every_replay_field() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), PeriCaps::default());

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), Arc::clone(&fixture.sessions), None).await;

    client
        .send_request("initialize", s9c_peri_initialize_params())
        .await
        .expect("peri 客户端的 initialize 必须成功");
    let reply = client
        .send_request("session/load", json!({ "sessionId": fixture.sid }))
        .await
        .expect("协商过 peri 能力的连接，session/load 必须成功");
    let (texts, keys, metas) = s9c_wire_facts(&client, &reply).await;
    task.abort();

    assert!(
        texts.len() >= 3,
        "协商连接回放的是热态有效集合，正文非空: {texts:?}"
    );
    assert!(
        reply["nextCursor"].is_string() && reply["snapshotToken"].is_string(),
        "协商过分页的连接，响应必须带首游标与快照身份: {reply}"
    );
    let generation = reply["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("协商过回放能力的连接，响应必须带代次");
    assert!(
        !metas.is_empty(),
        "回放通知必须带 meta（否则本用例是空转）: {keys:?}"
    );
    for meta in &metas {
        assert_eq!(
            meta["periReplay"],
            Value::Bool(true),
            "协商过回放能力的连接，每条回放通知都必须带 periReplay: {meta}"
        );
        assert_eq!(
            meta["periLoadGeneration"],
            Value::String(generation.to_owned()),
            "通知与响应必须同源代次（REQ-H04）: {meta}"
        );
        assert!(
            meta["periEntryId"].is_string(),
            "协商过分页的连接，条目身份必须照旧打上: {meta}"
        );
    }
}

/// 用例 C（复核者新增）：连接**从未 `initialize`**（进程内 TUI / MPSC 路径），会话级注册表
/// 是 `PeriCaps::default()`——即历史语义完全靠连接兜底决定。
///
/// 断言：回放标记照旧（历史全能力语义不缩水），且分页准入仍失败关闭（一个分页字段都没有）。
#[tokio::test]
#[serial]
async fn s9c_verification_a_never_initialized_in_process_connection_keeps_the_replay_marks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    fixture.archive(1).await;
    let canonical: Vec<String> = fixture
        .cfg
        .session_resources
        .load_session_history(&fixture.sid)
        .await
        .unwrap()
        .iter()
        .filter_map(|payload| payload.as_message().map(|message| message.content()))
        .collect();
    // 注册表刻意留空能力：本用例的标记若出现，只可能来自连接的兜底全能力。
    fixture
        .cfg
        .session_manager
        .caps_registry()
        .insert(fixture.sid.clone(), PeriCaps::default());

    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), Arc::clone(&fixture.sessions), None).await;

    let reply = client
        .send_request("session/load", json!({ "sessionId": fixture.sid }))
        .await
        .expect("进程内路径的 session/load 必须成功");
    let (texts, keys, metas) = s9c_wire_facts(&client, &reply).await;
    task.abort();

    let generation = reply["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("未 initialize 的进程内路径保持历史全能力语义：响应仍带代次")
        .to_owned();
    assert!(
        metas
            .iter()
            .any(|meta| meta["periReplay"] == Value::Bool(true)),
        "未 initialize 的进程内路径仍必须收到 periReplay: {metas:?}"
    );
    for meta in &metas {
        assert_eq!(
            meta["periLoadGeneration"],
            Value::String(generation.clone()),
            "通知与响应必须同源代次: {meta}"
        );
    }
    for forbidden in ["nextCursor", "snapshotToken", "periEntryId", "periPosition"] {
        assert!(
            !keys.iter().any(|key| key == forbidden),
            "未协商分页的连接不得出现分页字段 {forbidden}: {keys:?}"
        );
    }
    assert_eq!(
        texts, canonical,
        "未协商分页的进程内路径仍走完整 canonical 回放"
    );
}

// ─── A15 回归：own 区首条是不可展示条目（subagent / fork / bg 子会话的真实形状）──────
//
// 曾经：`dispatch::initial_history_cursor` 取「有效视图最早一条 own payload」当锚点。
// 该条目若不可展示、又不被 Full 排除（真实来源：子会话身份 System 条目，
// `session/subagent/factory/spawn.rs:351`），比它晚的被排除原文永远不会被任何一页交付；
// 它同时又是线程最旧一行时，`nextCursor` 直接是 `null`（谎称没有更早历史）。
// 锚点是**首屏已交付集合**的下边界，可展示性判据与分页出口同源。

/// own = [System 身份, h1, h2] + 一次 Full（排除 h1/h2、追加摘要）：
/// 锚点落在摘要（首屏唯一可展示条目），压缩原文随页链全部交付。
#[tokio::test]
#[serial]
async fn the_initial_cursor_anchors_past_non_displayable_own_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = create_bound_fixture(
        &fixture.cfg,
        tmp.path().to_str().unwrap(),
        Some("a15-system-identity-head"),
    )
    .await;

    let system = PersistedPayload::Message(BaseMessage::system("identity"));
    let h1 = PersistedPayload::Message(BaseMessage::human("older turn"));
    let h2 = PersistedPayload::Message(BaseMessage::ai("just-compacted answer"));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[system.clone(), h1.clone(), h2.clone()])
        .await
        .unwrap();
    fixture
        .cfg
        .session_resources
        .apply_compaction(
            &sid,
            &CompactionChange {
                flag_updates: vec![
                    (
                        h1.id(),
                        MessageFlags {
                            excluded: true,
                            ..Default::default()
                        },
                    ),
                    (
                        h2.id(),
                        MessageFlags {
                            excluded: true,
                            ..Default::default()
                        },
                    ),
                ],
                appended_messages: vec![BaseMessage::ai("compacted summary")],
            },
        )
        .await
        .unwrap();

    let effective = fixture
        .cfg
        .session_resources
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert_eq!(
        effective.first().map(PersistedPayload::id),
        Some(system.id()),
        "夹具：身份 System 条是 own 区首条（子会话形状）"
    );
    let summary = effective.last().map(PersistedPayload::id);
    assert_eq!(effective.len(), 2, "有效视图 = [身份 System, 摘要]");

    let initial = crate::dispatch::initial_history_cursor(&fixture.cfg.controller, &sid)
        .await
        .unwrap();
    let cursor = initial
        .next_cursor
        .expect("存储里有压缩原文，不得谎称没有更早历史");
    assert_eq!(
        Some(cursor.anchor),
        summary,
        "锚点是首屏唯一可展示条目（摘要），不是身份 System 条"
    );

    // 走完页链：压缩原文（比锚点早的行）必须全部交付，且与首屏零重叠。
    let mut next = Some(cursor);
    let mut delivered: Vec<MessageId> = Vec::new();
    for _ in 0..4 {
        let Some(current) = next else { break };
        let page = crate::dispatch::load_history_page(
            fixture.cfg.controller.as_ref(),
            &sid,
            &current,
            50,
            history_page::PAGE_BYTE_BUDGET,
        )
        .await
        .expect("初始游标必须可用");
        delivered.extend(page.items.iter().map(|item| item.id));
        next = page.next_cursor;
    }
    assert!(next.is_none(), "页链读到范围起点收尾，不假造终点");
    assert_eq!(delivered, vec![h1.id(), h2.id()], "压缩原文按历史正序交付");
    assert!(
        !delivered.contains(&summary.expect("有效视图非空")),
        "首屏已展示的摘要不得重复出现在页里"
    );
}

/// 有效视图里一条可展示条目都没有（只有身份 System，且没有任何归档）：`nextCursor = null`。
/// 这一支走「退回最新一条有效自有 payload」的兜底锚点，它同样不能编造游标。
#[tokio::test]
#[serial]
async fn an_all_non_displayable_own_region_reports_no_earlier_page() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = create_bound_fixture(
        &fixture.cfg,
        tmp.path().to_str().unwrap(),
        Some("a15-system-only"),
    )
    .await;
    let system = PersistedPayload::Message(BaseMessage::system("identity"));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[system])
        .await
        .unwrap();

    let initial = crate::dispatch::initial_history_cursor(&fixture.cfg.controller, &sid)
        .await
        .unwrap();
    assert!(
        initial.next_cursor.is_none(),
        "唯一自有行既是锚点又是范围起点：没有更早历史"
    );
}

/// 兜底分支的证据（不是「为了有个游标」而编造）：有效视图里一条可展示条目都没有、而存储里
/// 还有更老的可展示归档行时，锚点落在**最新一条有效自有条目**上，那行归档正文仍可交付
/// （旧实现的 `payloads.first()` 在这里直接 `nextCursor = null`，归档行不可达）。
#[tokio::test]
#[serial]
async fn the_fallback_anchor_keeps_an_archived_row_reachable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = create_bound_fixture(
        &fixture.cfg,
        tmp.path().to_str().unwrap(),
        Some("a15-fallback-anchor"),
    )
    .await;

    let system = PersistedPayload::Message(BaseMessage::system("identity"));
    let archived = PersistedPayload::Message(BaseMessage::human("archived body"));
    let reminder = model_only_reminder("model-only note");
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[system.clone(), archived.clone(), reminder.clone()])
        .await
        .unwrap();
    // 只排除归档行、且**不**追加摘要：「摘要被移除后正文仍被排除」这一形状的极端版。
    fixture
        .cfg
        .session_resources
        .apply_compaction(
            &sid,
            &CompactionChange {
                flag_updates: vec![(
                    archived.id(),
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                )],
                appended_messages: vec![],
            },
        )
        .await
        .unwrap();

    let effective = fixture
        .cfg
        .session_resources
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert_eq!(
        effective
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![system.id(), reminder.id()],
        "形状前提：有效视图 = [身份 System, 仅模型受众 reminder]"
    );
    assert!(
        effective
            .iter()
            .all(|payload| !payload.displayable_to_client()),
        "形状前提：有效视图里没有任何可展示条目"
    );

    let initial = crate::dispatch::initial_history_cursor(&fixture.cfg.controller, &sid)
        .await
        .unwrap();
    let cursor = initial
        .next_cursor
        .expect("存储里还有一条更老的可展示行，不得谎称没有更早历史");
    assert_eq!(
        cursor.anchor,
        reminder.id(),
        "兜底锚点是最新一条有效自有条目，不是有效视图第一条"
    );

    let page = crate::dispatch::load_history_page(
        fixture.cfg.controller.as_ref(),
        &sid,
        &cursor,
        50,
        history_page::PAGE_BYTE_BUDGET,
    )
    .await
    .expect("兜底游标必须可用");
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![archived.id()],
        "比兜底锚点更早的归档正文仍被交付"
    );
    assert!(page.next_cursor.is_none(), "读到范围起点收尾，不假造终点");
}

/// 仅模型受众的 canonical reminder：`displayable_to_client()` = false（不进入客户端渲染）。
fn model_only_reminder(body: &str) -> PersistedPayload {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("history_page_wire_test".into()),
            kind: "model_only".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: body.to_owned(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder,
    }
}
