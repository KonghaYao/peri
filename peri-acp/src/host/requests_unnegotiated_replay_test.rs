//! S9b：未协商 `peri.historyPagination` 的连接交付 **canonical 完整历史**（含 Full 归档
//! 原文），且是**流式**逐段读取（REQ §5.3）。
//!
//! 本文件覆盖的判据全部落在 host 层的请求出口上（`handle_request` / 带连接事实的
//! `handle_request_for_connection`）：wire 形状、交付内容、有界读取、失败关闭与回退。
//! 只有两条读取被替身替换（向前段读取与全量读取），其余逐项转发真实 SQLite 门面——
//! 「没调全量读取」这条断言只有在生产后端路径上才有意义。
//!
//! 夹具风格沿用 `requests_history_lifecycle_test.rs` / `requests_rewind_cases_test.rs`。

use super::*;
use async_trait::async_trait;
use peri_acp_types::history_page::{HistoryForwardCursor, HistorySegment};
use peri_acp_types::messages::{BaseMessage, ContentBlock, MessageId, ToolCallRequest};
use peri_acp_types::session_resources::{
    ActiveSessionSnapshot, BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot,
    CloseSettlement, ForkSnapshot, FrozenSnapshotBytes, NewSession, NewSessionDraft,
    PersistenceRecovery, RewindBoundary, RewindOutcome, SessionAvailability, SessionInitialization,
    SessionMetaPatch, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
    SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use peri_acp_types::PeriCaps;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::dispatch::session_replay::{REPLAY_SEGMENT_BYTE_BUDGET, REPLAY_SEGMENT_ROWS};
use crate::host::connection::ConnectionFacts;

// ── 记录调用的门面替身 ────────────────────────────────────────────────────────

/// 一次向前段读取的调用事实。
#[derive(Clone, Debug, Default)]
struct SegmentCall {
    /// 游标里的 `(after, tail)`；首段为 `None`。
    cursor: Option<(String, String)>,
    limit: usize,
    byte_budget: usize,
    /// 本段返回的 payload 条数（失败时为 0）。
    payloads: usize,
    error: Option<String>,
}

/// 替身给向前段读取的应答。
enum SpyMode {
    /// 转发真实门面（生产后端路径）。
    Forward,
    /// 后端没有这条读取：显式能力缺失。
    Unsupported,
    /// 读失败（损坏）。
    Corrupt,
    /// 第二次读取之前锚点行被并发删掉：真实存储据此失败关闭。
    AnchorVanishes,
    /// 后端没有向前段读取（显式能力缺失），且回退用的全量 canonical 读取也失败。
    /// 供独立复核用例验证「回退不得误报成功」。
    UnsupportedWithFailingFullRead,
    /// 首段由脚本给出（继承区组合的替身），此后结束。
    ScriptedFirstSegment(Vec<PersistedPayload>),
}

/// 只替换两条历史读取，其余逐项转发真实门面（注入点唯一，故用宏而不是每个方法各写一遍）。
struct ReplaySpy {
    inner: Arc<dyn SessionResources>,
    mode: SpyMode,
    segment_calls: Mutex<Vec<SegmentCall>>,
    full_reads: AtomicUsize,
    anchor_removed: AtomicBool,
}

macro_rules! impl_session_resources_with_segment_hooks {
    (
        $( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*
    ) => {
        #[async_trait]
        impl SessionResources for ReplaySpy {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            /// 注入点：向前段读取。
            async fn load_history_segment(
                &self,
                id: &ThreadId,
                cursor: Option<&HistoryForwardCursor>,
                limit: usize,
                byte_budget: usize,
            ) -> SessionResourceResult<HistorySegment> {
                self.segment(id, cursor, limit, byte_budget).await
            }

            /// 只计数：未协商路径不该走到全量读取（`Unsupported` 回退除外）。
            async fn load_session_history(
                &self,
                id: &ThreadId,
            ) -> SessionResourceResult<Vec<PersistedPayload>> {
                self.full_reads.fetch_add(1, Ordering::SeqCst);
                // 独立复核对例：段读取缺失时的回退读取也失败，调用方必须让 load 失败。
                if matches!(&self.mode, SpyMode::UnsupportedWithFailingFullRead) {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Corrupt {
                            detail: "injected: full canonical read fails too".to_owned(),
                        },
                    ));
                }
                self.inner.load_session_history(id).await
            }
        }
    };
}

impl_session_resources_with_segment_hooks! {
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &std::path::Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_active_snapshot(id: &ThreadId) -> ActiveSessionSnapshot;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    load_entry_positions(id: &ThreadId) -> HashMap<MessageId, u64>;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary, clear_excluded: &[MessageId]) -> RewindOutcome;
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}

impl ReplaySpy {
    fn new(inner: Arc<dyn SessionResources>, mode: SpyMode) -> Self {
        Self {
            inner,
            mode,
            segment_calls: Mutex::new(Vec::new()),
            full_reads: AtomicUsize::new(0),
            anchor_removed: AtomicBool::new(false),
        }
    }

    fn segment_calls(&self) -> Vec<SegmentCall> {
        self.segment_calls.lock().unwrap().clone()
    }

    fn full_reads(&self) -> usize {
        self.full_reads.load(Ordering::SeqCst)
    }

    /// 注入点：向前段读取。
    async fn segment(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryForwardCursor>,
        limit: usize,
        byte_budget: usize,
    ) -> SessionResourceResult<HistorySegment> {
        let outcome = match &self.mode {
            SpyMode::Forward => {
                self.inner
                    .load_history_segment(id, cursor, limit, byte_budget)
                    .await
            }
            SpyMode::Unsupported | SpyMode::UnsupportedWithFailingFullRead => Err(
                SessionResourceError::new(SessionResourceErrorKind::Unsupported),
            ),
            SpyMode::Corrupt => Err(SessionResourceError::new(
                SessionResourceErrorKind::Corrupt {
                    detail: "injected: canonical history segment is corrupt".to_owned(),
                },
            )),
            SpyMode::AnchorVanishes => {
                if cursor.is_some() && !self.anchor_removed.swap(true, Ordering::SeqCst) {
                    // 续读之前把该线程的行全部精确删除：锚点消失，真实存储必须失败关闭。
                    let snapshot = self.inner.load_session_snapshot(id).await?;
                    let ids: Vec<MessageId> =
                        snapshot.payloads.iter().map(PersistedPayload::id).collect();
                    self.inner.remove_history_entries(id, &ids).await?;
                }
                self.inner
                    .load_history_segment(id, cursor, limit, byte_budget)
                    .await
            }
            SpyMode::ScriptedFirstSegment(payloads) => {
                assert!(cursor.is_none(), "脚本只给首段");
                Ok(HistorySegment {
                    payloads: payloads.clone(),
                    next_cursor: None,
                })
            }
        };
        let mut call = SegmentCall {
            cursor: cursor.map(|cursor| (cursor.after.clone(), cursor.tail.clone())),
            limit,
            byte_budget,
            payloads: 0,
            error: None,
        };
        match &outcome {
            Ok(segment) => call.payloads = segment.payloads.len(),
            Err(error) => call.error = Some(error.to_string()),
        }
        self.segment_calls.lock().unwrap().push(call);
        outcome
    }
}

// ── 夹具 ─────────────────────────────────────────────────────────────────────

struct ReplayFixture {
    cfg: AcpServerConfig,
    spy: Arc<ReplaySpy>,
    sessions: HashMap<String, SessionState>,
    transport: Arc<MockTransport>,
    id: String,
    /// 本次请求携带的**连接**协商能力（`ConnectionFacts.caps`）。回放标记 / 代次戳的唯一门。
    ///
    /// 默认全 false = 一条从未协商任何 peri 扩展的连接；要验证「声明了 `peri.replay` 就
    /// 有标记」的用例必须显式 [`Self::set_connection_caps`]。
    connection_caps: PeriCaps,
    _tmp: tempfile::TempDir,
    cwd: PathBuf,
}

impl ReplayFixture {
    /// 真库 + 门面替身：`cfg.session_resources` 与 `cfg.controller` 都只持这一份替身。
    async fn new(mode: SpyMode) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(tmp.path()).unwrap().join("saved");
        std::fs::create_dir(&cwd).unwrap();
        let config = make_peri_config_with_provider(make_provider_config(
            "local",
            "openai",
            "fixture-key",
            "fixture-model",
        ));
        let provider = LlmProvider::from_config(&config).unwrap();
        let (_bridge, facade) = peri_resources::sessions::open_store_and_facade_for_tests(
            tmp.path().join("threads.db"),
        )
        .await
        .unwrap();
        let spy = Arc::new(ReplaySpy::new(Arc::new(facade), mode));
        let resources: Arc<dyn SessionResources> = spy.clone();
        let cfg = build_server_config(config, provider, &tmp, resources).await;
        Self {
            cfg,
            spy,
            sessions: HashMap::new(),
            transport: Arc::new(MockTransport::default()),
            id: String::new(),
            connection_caps: PeriCaps::default(),
            _tmp: tmp,
            cwd,
        }
    }

    /// 只在存储里建一条可 load 的会话（热态不持有它 ⇒ 恢复走冷路径）。
    async fn create_session(&mut self, sid: &str) {
        let workspace = self
            .cfg
            .session_resources
            .resolve_workspace(&self.cwd)
            .await
            .unwrap();
        let frozen = self
            .cfg
            .session_manager
            .build_frozen_data(self.cwd.to_str().expect("fixture cwd is UTF-8"));
        let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
        self.cfg
            .session_resources
            .create_session(&bound_input(sid, &workspace, encoded))
            .await
            .unwrap();
        self.id = sid.to_owned();
    }

    async fn append(&self, messages: &[BaseMessage]) {
        let payloads: Vec<PersistedPayload> = messages
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect();
        self.cfg
            .session_resources
            .append_history(&self.id, &payloads)
            .await
            .unwrap();
    }

    /// Full Compact 的存储侧形状：这些行被标成 `excluded`（原文仍可读出，不参与有效集合）。
    async fn archive(&self, ids: &[MessageId]) {
        let excluded = MessageFlags {
            excluded: true,
            ..Default::default()
        };
        let updates: Vec<(MessageId, MessageFlags)> =
            ids.iter().map(|id| (*id, excluded.clone())).collect();
        self.cfg
            .session_resources
            .apply_message_projections(&self.id, &updates)
            .await
            .unwrap();
    }

    /// 会话级 caps 注册表声明了什么：**只**影响命令表广播等会话级面（`ensure_session_caps`
    /// 语义）。回放标记 / 代次戳从 S9c 起按**当前连接**判定（`ConnectionFacts.caps`，见
    /// [`Self::set_connection_caps`]），因此这里写什么都不改变回放通知的形状——夹具的用法
    /// 正是让「注册表里存着别的连接写下的值」这类场景可写（REQ §5.3、验收 A25）。
    fn set_caps(&self, caps: PeriCaps) {
        self.cfg
            .session_manager
            .caps_registry()
            .insert(self.id.clone(), caps);
    }

    /// 本次**连接**声明了什么（`ConnectionFacts.caps`）：回放标记与代次戳的门。
    fn set_connection_caps(&mut self, caps: PeriCaps) {
        self.connection_caps = caps;
    }

    /// 未协商分页的连接：分页准入为 false，回放标记按本连接声明的 caps。
    async fn load_unnegotiated(&mut self) -> Result<Value, AcpError> {
        self.load_with_pagination(false).await
    }

    /// 协商分页的连接：分页准入为 true，回放标记同样按本连接声明的 caps。
    async fn load_negotiated(&mut self) -> Result<Value, AcpError> {
        self.load_with_pagination(true).await
    }

    async fn load_with_pagination(&mut self, history_pagination: bool) -> Result<Value, AcpError> {
        let facts = ConnectionFacts {
            history_pagination,
            caps: self.connection_caps.clone(),
        };
        self.request("session/load", facts).await
    }

    async fn request(&mut self, method: &str, facts: ConnectionFacts) -> Result<Value, AcpError> {
        let transport: Arc<dyn crate::transport::AcpTransport> = self.transport.clone();
        let params = json!({ "sessionId": self.id });
        crate::host::requests::handle_request_for_connection(
            method,
            &params,
            &self.cfg,
            &mut self.sessions,
            &transport,
            facts,
        )
        .await
    }

    /// 存储侧 canonical 完整历史（含归档原文）——回放的期望来源。
    async fn canonical(&self) -> Vec<PersistedPayload> {
        self.cfg
            .session_resources
            .load_session_history(&self.id)
            .await
            .unwrap()
    }

    async fn effective(&self) -> Vec<PersistedPayload> {
        self.cfg
            .session_resources
            .load_active_snapshot(&self.id)
            .await
            .unwrap()
            .payloads
    }

    /// 清掉夹具自身读取留下的记录：`canonical()` / `effective()` 也走这份门面替身，
    /// 断言「本次 load 读了几次」必须先清账。
    fn reset_reads(&self) {
        self.spy.segment_calls.lock().unwrap().clear();
        self.spy.full_reads.store(0, Ordering::SeqCst);
    }

    /// 全部 `session/update` 通知（含回放之外的 configOptions / 命令广播）。
    fn session_updates(&self) -> Vec<Value> {
        self.transport
            .notifications()
            .into_iter()
            .filter(|(method, _)| method == "session/update")
            .map(|(_, params)| params)
            .collect()
    }

    /// 回放通知：`session/update` 里逐条投递的文本 chunk（顺序即 wire 顺序）。
    ///
    /// load 的响应前后还会广播 configOptions / 命令表，它们不是回放内容；本夹具的会话只有
    /// 文本消息，因此回放面的判据 = 文本 chunk 序列。
    fn updates(&self) -> Vec<Value> {
        self.session_updates()
            .into_iter()
            .filter(|params| {
                matches!(
                    params["update"]["sessionUpdate"].as_str(),
                    Some("user_message_chunk") | Some("agent_message_chunk")
                )
            })
            .collect()
    }

    /// 回放通知里逐条的正文。
    fn texts(&self) -> Vec<String> {
        self.updates()
            .iter()
            .map(|params| {
                params["update"]["content"]["text"]
                    .as_str()
                    .expect("text chunk carries content.text")
                    .to_owned()
            })
            .collect()
    }
}

/// 一条消息的正文（本夹具只放文本消息）。
fn payload_text(payload: &PersistedPayload) -> String {
    payload
        .as_message()
        .expect("fixture payloads are messages")
        .content()
}

fn payload_ids(payloads: &[PersistedPayload]) -> Vec<MessageId> {
    payloads.iter().map(PersistedPayload::id).collect()
}

/// JSON 树里出现过的全部键（含 `_meta` 内层）：wire 级「一个 Peri 私有字段都没有」的判据是
/// 前缀而不是枚举字段名。
fn collect_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                out.push(key.clone());
                collect_keys(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_keys(item, out)),
        _ => {}
    }
}

fn all_keys(value: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    collect_keys(value, &mut keys);
    keys
}

/// 回放通知里出现的全部键（顺序即 wire 顺序）。
fn replay_keys(fixture: &ReplayFixture) -> Vec<String> {
    fixture.updates().iter().flat_map(all_keys).collect()
}

/// 全部 `session/update` 通知 + 响应里出现的全部键：分页私有**字段名**的全局判据
/// （命令表 / 配置项等回放之外的投影也一并扫，避免它们成为分页语义的旁路出口）。
fn wire_keys(fixture: &ReplayFixture, response: &Value) -> Vec<String> {
    let mut keys: Vec<String> = fixture
        .session_updates()
        .iter()
        .flat_map(all_keys)
        .collect();
    keys.extend(all_keys(response));
    keys
}

/// 分页私有字段名（本切片必须一个都不引入）。
const PAGINATION_PRIVATE_FIELDS: [&str; 4] =
    ["nextCursor", "snapshotToken", "periEntryId", "periPosition"];

fn assert_no_pagination_fields(keys: &[String]) {
    for forbidden in PAGINATION_PRIVATE_FIELDS {
        assert!(
            !keys.iter().any(|key| key == forbidden),
            "分页私有字段 {forbidden} 不得出现在未协商连接的 wire 上: {keys:?}"
        );
    }
}

// ── 用例 ─────────────────────────────────────────────────────────────────────

/// 未协商连接 + 含 Full 归档（excluded 行）的会话：回放**包含归档原文**，且逐条内容与顺序
/// 等于 `load_session_history` 的 canonical 序列。
#[tokio::test]
async fn streamed_canonical_replay_delivers_the_archived_bodies_in_canonical_order() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-archive").await;
    fixture
        .append(&[
            BaseMessage::human("归档：第一轮问题"),
            BaseMessage::ai("归档：第一轮回答"),
            BaseMessage::human("有效：第二轮问题"),
            BaseMessage::ai("有效：第二轮回答"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    fixture.archive(&payload_ids(&canonical)[..2]).await;
    fixture.set_caps(PeriCaps::default());

    let effective = fixture.effective().await;
    assert_eq!(
        effective.len(),
        2,
        "夹具前提：归档后有效集合只剩两条（回放不得以它为来源）"
    );
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    let expected: Vec<String> = canonical.iter().map(payload_text).collect();
    assert_eq!(expected.len(), 4);
    assert!(
        expected.contains(&"归档：第一轮问题".to_owned()),
        "夹具前提：canonical 读取含归档原文"
    );
    assert_eq!(
        fixture.texts(),
        expected,
        "回放必须逐条交付 canonical 完整历史（含归档原文），顺序一致"
    );
    assert_eq!(fixture.spy.segment_calls().len(), 1, "四条一段读完");
    assert_eq!(fixture.spy.full_reads(), 0, "生产后端路径不读全量");
}

/// wire 级：标准客户端（什么都没声明）的初始 load 回放通知里**一个 Peri 私有字段都没有**，
/// 且全量通知与响应里不出现任何分页私有字段名。
///
/// 判据是**前缀 / 字段名**而不是枚举：回放通知上不设任何枚举白名单，只要出现 `peri*` 就失败。
/// （响应里既有的非分页扩展，例如 `_meta.peri.sessionWorkspaceV1`，属于别的切片，不在本判据内。）
#[tokio::test]
async fn streamed_canonical_replay_wire_carries_no_peri_private_fields_for_a_standard_client() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-wire").await;
    fixture
        .append(&[BaseMessage::human("首屏问题"), BaseMessage::ai("首屏回答")])
        .await;
    fixture.set_caps(PeriCaps::default());

    let response = fixture.load_unnegotiated().await.unwrap();

    let keys = replay_keys(&fixture);
    assert!(!keys.is_empty(), "回放通知上确实有内容可查");
    let peri: Vec<&String> = keys.iter().filter(|key| key.starts_with("peri")).collect();
    assert!(
        peri.is_empty(),
        "未协商连接的回放通知不得出现 Peri 私有字段: {peri:?}"
    );
    assert_no_pagination_fields(&wire_keys(&fixture, &response));
    assert!(
        !wire_keys(&fixture, &response)
            .iter()
            .any(|key| key == "periLoadGeneration"),
        "未声明 peri.replay 的连接不会拿到代次"
    );
    assert_eq!(fixture.texts(), vec!["首屏问题", "首屏回答"]);
}

/// `periLoadGeneration` 的门是 `caps.replay`（回放扩展），**不是**分页能力：这是本切片之前
/// 就成立的契约（`docs/design/peri-acp-protocol.md`：代次「在协商过 `replay` 的连接上」以
/// `_meta.periLoadGeneration` 出现），本切片没有改动它。
///
/// 因此「声明了 `peri.replay`、没声明 `peri.historyPagination`」的连接仍有代次，但**没有**
/// 任何分页私有字段（条目身份 / 位置 / 游标 / 快照身份）。
///
/// `caps.replay` 取自**当前连接**（S9c 起）：本用例显式声明连接 caps，同时刻意把会话级
/// 注册表写成 `PeriCaps::default()`——门若回读注册表，这里就会一条标记都收不到。
#[tokio::test]
async fn streamed_canonical_replay_keeps_the_generation_gated_by_the_replay_capability() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture
        .create_session("streamed-canonical-generation")
        .await;
    fixture
        .append(&[BaseMessage::human("代次问题"), BaseMessage::ai("代次回答")])
        .await;
    fixture.set_connection_caps(PeriCaps {
        replay: true,
        ..PeriCaps::default()
    });
    fixture.set_caps(PeriCaps::default());

    let response = fixture.load_unnegotiated().await.unwrap();

    let generation = response["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("协商过 replay 的连接在响应里拿到代次")
        .to_owned();
    let updates = fixture.updates();
    assert_eq!(updates.len(), 2);
    for params in &updates {
        let meta = &params["update"]["_meta"];
        assert_eq!(meta["periReplay"], Value::Bool(true));
        assert_eq!(
            meta["periLoadGeneration"],
            Value::String(generation.clone())
        );
        assert!(
            meta.get("periEntryId").is_none() && meta.get("periPosition").is_none(),
            "未协商分页的连接不得到条目身份 / 位置戳: {meta}"
        );
    }
    // 代次之外一个分页私有字段都没有：声明了回放扩展 ≠ 拿到分页语义。
    assert_no_pagination_fields(&wire_keys(&fixture, &response));
}

/// 有界证据：段读取被多次调用，**没有任何一次返回全量**，且未协商路径不调用全量读取。
#[tokio::test]
async fn streamed_canonical_replay_reads_bounded_segments_and_never_the_full_history() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-bounded").await;
    let messages: Vec<BaseMessage> = (0..REPLAY_SEGMENT_ROWS + 10)
        .map(|index| {
            if index % 2 == 0 {
                BaseMessage::human(format!("第 {index} 条"))
            } else {
                BaseMessage::ai(format!("第 {index} 条回答"))
            }
        })
        .collect();
    fixture.append(&messages).await;
    fixture.set_caps(PeriCaps::default());
    let total = fixture.canonical().await.len();
    assert_eq!(total, messages.len());
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    let calls = fixture.spy.segment_calls();
    assert!(
        calls.len() >= 2,
        "越过一段规模的历史必须分多段读取，实际 {} 次",
        calls.len()
    );
    assert!(
        calls.iter().all(|call| call.payloads < total),
        "没有任何一次读取允许返回全量（total = {total}）: {calls:?}"
    );
    assert!(
        calls.iter().all(|call| call.limit == REPLAY_SEGMENT_ROWS
            && call.byte_budget == REPLAY_SEGMENT_BYTE_BUDGET),
        "段规模必须是命名的回放常量: {calls:?}"
    );
    assert!(
        calls[0].cursor.is_none() && calls[1..].iter().all(|call| call.cursor.is_some()),
        "首段无游标，后续段必须带游标: {calls:?}"
    );
    assert_eq!(fixture.spy.full_reads(), 0, "流式路径不读全量");
    assert_eq!(fixture.texts().len(), total, "逐条交付，无重复无遗漏");
}

/// 失败关闭：段读取损坏 → 本次 load 以错误结束，且**不产生半份首屏 + 成功响应**。
#[tokio::test]
async fn streamed_canonical_replay_failure_closes_the_load() {
    let mut fixture = ReplayFixture::new(SpyMode::Corrupt).await;
    fixture.create_session("streamed-canonical-corrupt").await;
    fixture
        .append(&[BaseMessage::human("不会交付"), BaseMessage::ai("不会交付")])
        .await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    let error = fixture
        .load_unnegotiated()
        .await
        .expect_err("段读取失败必须让本次 load 失败");
    assert_eq!(error.code, -32603, "错误码: {error:?}");
    assert!(
        fixture.updates().is_empty(),
        "首段就失败：一条通知都不该送出"
    );
    assert_eq!(fixture.spy.full_reads(), 0);
    assert_eq!(
        fixture.canonical().await.len(),
        2,
        "失败的交付不改动存储事实"
    );
}

/// 锚点消失（回放途中的并发精确删除）同样失败关闭：不允许把空段当成「回放结束」。
#[tokio::test]
async fn streamed_canonical_replay_fails_closed_when_the_anchor_vanishes_mid_stream() {
    let mut fixture = ReplayFixture::new(SpyMode::AnchorVanishes).await;
    fixture.create_session("streamed-canonical-anchor").await;
    let messages: Vec<BaseMessage> = (0..REPLAY_SEGMENT_ROWS + 5)
        .map(|index| BaseMessage::human(format!("第 {index} 条")))
        .collect();
    fixture.append(&messages).await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    let error = fixture
        .load_unnegotiated()
        .await
        .expect_err("锚点消失必须让本次 load 失败");
    assert_eq!(error.code, -32603, "错误码: {error:?}");

    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 2, "首段成功、续读失败: {calls:?}");
    assert!(calls[0].error.is_none());
    assert!(
        calls[1].error.is_some(),
        "第二次读取必须带着真实存储的失败回来: {calls:?}"
    );
    assert!(
        !fixture.texts().is_empty() && fixture.texts().len() < messages.len(),
        "失败发生在半途（这正是「不得留下半份首屏 + 成功响应」的场景）"
    );
}

/// 回退：后端没有向前段读取时**显式**回退到全量 canonical 读取，回放仍然完整。
///
/// 判据是**调用记录**（首段读过一次 + 全量读取恰好一次）与交付内容，不是日志文本：生产代码
/// 那条 warn 是诊断，把它断言下来要依赖 tracing 的**进程级** callsite 兴趣缓存与线程局部
/// 订阅（`set_default` + `rebuild_interest_cache`），默认并发下会被同进程其它用例的
/// `rebuild_interest_cache` / 订阅装卸搅掉（`.recon/S9-boundedness.md` 第 6 节 R8：同一条
/// 命令首跑失败、复跑通过，产品断言两次都通过）。回退本身的行为由下面三条断言钉住。
#[tokio::test]
async fn streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported(
) {
    let mut fixture = ReplayFixture::new(SpyMode::Unsupported).await;
    fixture.create_session("streamed-canonical-fallback").await;
    fixture
        .append(&[
            BaseMessage::human("回退：归档问题"),
            BaseMessage::ai("回退：归档回答"),
            BaseMessage::human("回退：有效问题"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    fixture.archive(&payload_ids(&canonical)[..2]).await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    let expected: Vec<String> = canonical.iter().map(payload_text).collect();
    assert_eq!(fixture.texts(), expected, "回退同样交付完整 canonical 历史");
    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 1, "回退只发生在首段之前");
    assert!(
        calls[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("behavior is unsupported here")),
        "首段那句的失败理由是「后端没有这条读取」（回退的触发条件）: {calls:?}"
    );
    assert_eq!(fixture.spy.full_reads(), 1, "回退经全量读取一次");
}

/// 边界：空会话（没有任何自有行）回放成功且不产出一条通知。
#[tokio::test]
async fn streamed_canonical_replay_of_an_empty_session_succeeds_without_notifications() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-empty").await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    assert!(fixture.texts().is_empty());
    assert_eq!(
        fixture.spy.segment_calls().len(),
        1,
        "空会话也走同一条首段读取，之后凭 next_cursor = None 结束"
    );
    assert_eq!(fixture.spy.full_reads(), 0);
}

/// 边界：只有继承区的会话（首段一次性给出继承区 payload）照常交付，顺序不变。
///
/// 脚本只替代「存储侧把继承区装配进首段」这一步（S9a 的段契约，由 peri-resources 的段用例
/// 覆盖）；本用例验证的是 ACP 出口对首段继承区 payload 的投影与顺序。
#[tokio::test]
async fn streamed_canonical_replay_delivers_an_inherited_only_segment() {
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("继承区问题")),
        PersistedPayload::Message(BaseMessage::ai("继承区回答")),
    ];
    let mut fixture = ReplayFixture::new(SpyMode::ScriptedFirstSegment(inherited.clone())).await;
    fixture.create_session("streamed-canonical-inherited").await;
    fixture.set_caps(PeriCaps::default());
    assert!(
        fixture.canonical().await.is_empty(),
        "夹具前提：本线程没有任何自有行"
    );
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    assert_eq!(fixture.texts(), vec!["继承区问题", "继承区回答"]);
    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].payloads, inherited.len());
    assert_eq!(fixture.spy.full_reads(), 0);
}

/// 边界：单条正文超预算时独占一段，仍被**完整**交付（完整回放不允许静默丢内容）。
#[tokio::test]
async fn streamed_canonical_replay_delivers_an_oversized_body_whole() {
    let huge = "x".repeat(530_000);
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("streamed-canonical-oversized").await;
    fixture
        .append(&[BaseMessage::human("短行"), BaseMessage::ai(huge.clone())])
        .await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    let texts = fixture.texts();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0], "短行");
    assert_eq!(texts[1].len(), huge.len(), "超大正文必须整条交付");
    assert_eq!(texts[1], huge);

    let calls = fixture.spy.segment_calls();
    assert_eq!(calls.len(), 2, "超预算的那条独占一段: {calls:?}");
    assert_eq!(calls[0].payloads, 1, "第一段只收下能进预算的那条");
    assert_eq!(calls[1].payloads, 1, "超大正文独占第二段");
}

/// `AlreadyLive`（会话已在内存）分支：未协商连接的 canonical 回放**同样从存储流式读取**，
/// 不得回退成只重放热态 `history_payloads`。夹具把热态有效集合缩到只剩尾条目：若回放源是
/// 内存，归档原文就会消失。
#[tokio::test]
async fn streamed_canonical_replay_of_a_live_session_still_reads_storage() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    let sid = register_session_with_history(
        &mut fixture.sessions,
        fixture.cwd.to_str().unwrap(),
        &fixture.cfg,
    )
    .await;
    fixture.id = sid.clone();
    fixture.set_caps(PeriCaps::default());
    let live_ids = payload_ids(&fixture.canonical().await);
    fixture.archive(&live_ids[..2]).await;
    let live_tail = fixture.sessions[&sid]
        .history_payloads
        .last()
        .unwrap()
        .clone();
    {
        let state = fixture.sessions.get_mut(&sid).unwrap();
        state.history_payloads = vec![live_tail];
        state.history = vec![state.history[2].clone()];
    }
    fixture.reset_reads();

    fixture.load_unnegotiated().await.unwrap();

    assert_eq!(
        fixture.texts(),
        vec!["第一轮用户问题", "第一轮回答", "第二轮用户问题"],
        "AlreadyLive 也必须交付存储里的 canonical 历史（含归档原文）"
    );
    assert_eq!(fixture.spy.full_reads(), 0);
}

/// 协商连接回归：有效集合 + 条目身份 / 位置戳 + 代次，语义与改动前一致
/// （未协商路径的改动不得泄漏到这条路径）。
///
/// 刻意走冷恢复（会话不在内存）：首屏与位置表才会同出于存储的有效视图，而不是夹具内存里
/// 那份尚未被归档改写的 `history_payloads`。
///
/// 声明在**连接**上（S9c 起的唯一门）：peri TUI 就是这样一条连接（协商分页与回放）。
#[tokio::test]
async fn negotiated_initial_load_keeps_the_effective_set_and_the_entry_marks() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture
        .create_session("streamed-canonical-negotiated")
        .await;
    fixture
        .append(&[
            BaseMessage::human("归档问题"),
            BaseMessage::ai("归档回答"),
            BaseMessage::human("有效问题"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    let live_id = canonical[2].id();
    fixture.archive(&payload_ids(&canonical)[..2]).await;
    fixture.set_connection_caps(PeriCaps {
        replay: true,
        history_pagination: true,
        ..PeriCaps::default()
    });
    fixture.reset_reads();

    let sid = fixture.id.clone();
    let response = fixture.load_negotiated().await.unwrap();

    let generation = response["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("协商连接在响应里拿到代次")
        .to_owned();
    assert_eq!(
        fixture.texts(),
        vec!["有效问题"],
        "协商连接回放的是**有效集合**（归档正文不重放）"
    );
    let updates = fixture.updates();
    assert_eq!(updates.len(), 1);
    let meta = &updates[0]["update"]["_meta"];
    assert_eq!(meta["periReplay"], Value::Bool(true));
    assert_eq!(meta["periLoadGeneration"], Value::String(generation));
    assert_eq!(meta["periEntryId"], format!("{sid}:{}", live_id.as_uuid()));
    assert!(
        meta["periPosition"].as_u64().is_some(),
        "协商连接的条目必须带存储序位置戳: {meta}"
    );
    assert_eq!(fixture.spy.full_reads(), 0, "协商路径不读全量 canonical");
    assert_eq!(
        fixture.spy.segment_calls().len(),
        0,
        "协商路径不走向前段读取"
    );
}

// ── S9-verification：独立复核用例（复核者加入，非实现者用例）─────────────────────

/// 未协商连接的初始 load 回放**逐条等于** `load_session_history` 的 canonical 输出。
///
/// 与实现者用例的差别（独立复核意图）：
/// - 载荷类型混合：human / AI blocks（reasoning + text + tool_use）/ AI tool_calls 与正文
///   并存 / tool 结果 / System；
/// - 归档行**交错**在中间（不是前缀），且被归档的是工具调用与工具结果；
/// - 期望序列在本用例内按 ACP 投影契约**独立重算**（不调用 `project_payload_updates`），
///   逐条比较 `messageId` / 正文 / 工具调用身份与顺序。
#[tokio::test]
async fn s9_verification_unnegotiated_replay_equals_canonical_history_for_mixed_payloads() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("s9-verification-mixed").await;
    fixture
        .append(&[
            BaseMessage::human("归档一：问题"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("归档一：思考"),
                ContentBlock::text("归档一：回答"),
                ContentBlock::tool_use("call-arch", "bash", json!({ "cmd": "ls" })),
            ]),
            BaseMessage::tool_result("call-arch", "归档一：工具结果"),
            BaseMessage::human("有效：第二个问题"),
            BaseMessage::ai_with_tool_calls(
                "有效：调用工具",
                vec![ToolCallRequest::new(
                    "call-live",
                    "read_file",
                    json!({ "path": "a.rs" }),
                )],
            ),
            BaseMessage::tool_result("call-live", "有效：工具结果"),
            BaseMessage::system("系统提示：不计入展示"),
            BaseMessage::ai("有效：最后回答"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    assert_eq!(canonical.len(), 8, "夹具前提：8 条持久化载荷");
    // 交错的归档段（工具调用 + 工具结果 + 一条 human），不是前缀。
    fixture.archive(&payload_ids(&canonical)[1..4]).await;
    fixture.set_caps(PeriCaps::default());

    let effective = fixture.effective().await;
    assert_eq!(
        effective.len(),
        5,
        "夹具前提：归档后有效集合只剩 m0 + m4..m7（回放不得以它为来源）"
    );
    assert_eq!(
        canonical
            .iter()
            .filter(|p| payload_text(p).starts_with("归档"))
            .count(),
        3,
        "夹具前提：canonical 读取含三条归档原文"
    );
    fixture.reset_reads();

    let response = fixture.load_unnegotiated().await.unwrap();

    let observed = observed_wire_facts(&fixture);
    let expected = expected_wire_facts(&canonical);
    assert_eq!(
        observed, expected,
        "未协商连接的回放序列必须逐条等于 canonical 输出（身份 / 正文 / 工具身份 / 顺序）"
    );
    assert_eq!(fixture.spy.full_reads(), 0, "流式路径不读全量");

    // 无私有语义：响应与全部通知里不得出现分页 / 代次私有字段（默认 caps 连接）。
    let response_keys = all_keys(&response);
    let notification_keys = replay_keys(&fixture);
    let private: Vec<&str> = response_keys
        .iter()
        .chain(notification_keys.iter())
        .map(String::as_str)
        .filter(|key| {
            matches!(
                *key,
                "nextCursor"
                    | "snapshotToken"
                    | "periEntryId"
                    | "periPosition"
                    | "periLoadGeneration"
                    | "periReplay"
            )
        })
        .collect();
    assert!(
        private.is_empty(),
        "默认 caps 的连接不得收到任何 Peri 私有字段: {private:?}"
    );
    assert!(
        response.get("nextCursor").is_none() && response.get("snapshotToken").is_none(),
        "未协商连接的成功响应不得携带分页字段: {response}"
    );
}

/// 回退路径也不得误报成功：后端没有向前段读取（`Unsupported`）且回退的全量 canonical
/// 读取同样失败时，本次 load 必须以 `-32603` 结束。
#[tokio::test]
async fn s9_verification_fallback_full_read_failure_still_closes_the_load() {
    let mut fixture = ReplayFixture::new(SpyMode::UnsupportedWithFailingFullRead).await;
    fixture
        .create_session("s9-verification-fallback-fails")
        .await;
    fixture
        .append(&[BaseMessage::human("问题"), BaseMessage::ai("回答")])
        .await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    let error = fixture
        .load_unnegotiated()
        .await
        .expect_err("回退读取也失败时必须让本次 load 失败");
    assert_eq!(error.code, -32603, "错误码: {error:?}");
    assert_eq!(fixture.spy.segment_calls().len(), 1, "先试段读取");
    assert_eq!(fixture.spy.full_reads(), 1, "再试全量回退读取");
    assert!(fixture.updates().is_empty(), "失败路径不得留下半份首屏");
}

/// 对照（§5.3 的另一半）：**协商过**分页的当前连接仍然拿到条目身份 / 位置戳与代次；
/// 同一份含归档行的夹具上，协商路径回放的是**有效集合**（归档原文不重放）。
///
/// 声明在**连接**上：门只看当前连接，会话级注册表里是别的连接留下的值也不影响（这里刻意
/// 留空 = `PeriCaps::default()`，若实现回读注册表，本用例的标记断言会失败）。
#[tokio::test]
async fn s9_verification_negotiated_connection_still_carries_the_entry_marks() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("s9-verification-negotiated").await;
    fixture
        .append(&[
            BaseMessage::human("归档：问题"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::text("归档：回答"),
                ContentBlock::tool_use("call-arch", "bash", json!({ "cmd": "ls" })),
            ]),
            BaseMessage::tool_result("call-arch", "归档：工具结果"),
            BaseMessage::human("有效：问题"),
            BaseMessage::ai("有效：回答"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    let archived: Vec<MessageId> = payload_ids(&canonical)[..3].to_vec();
    fixture.archive(&archived).await;
    fixture.set_connection_caps(PeriCaps {
        replay: true,
        history_pagination: true,
        ..PeriCaps::default()
    });
    fixture.reset_reads();

    let sid = fixture.id.clone();
    let response = fixture.load_negotiated().await.unwrap();

    let generation = response["_meta"]["periLoadGeneration"]
        .as_str()
        .expect("协商连接在响应里拿到代次")
        .to_owned();
    assert_eq!(
        fixture.texts(),
        vec!["有效：问题", "有效：回答"],
        "协商连接回放的是有效集合（归档原文不重放）"
    );
    let updates = fixture.updates();
    assert_eq!(updates.len(), 2, "有效集合只有两条可展示条目");
    for (params, payload) in updates.iter().zip(&canonical[3..]) {
        let meta = &params["update"]["_meta"];
        assert_eq!(meta["periReplay"], Value::Bool(true));
        assert_eq!(
            meta["periLoadGeneration"],
            Value::String(generation.clone())
        );
        assert_eq!(
            meta["periEntryId"],
            Value::String(format!("{sid}:{}", payload.id().as_uuid())),
            "协商连接的条目身份与历史页同一格式"
        );
        assert!(
            meta["periPosition"].as_u64().is_some(),
            "协商连接的条目必须带存储序位置戳: {meta}"
        );
    }
    for params in &updates {
        let entry = params["update"]["_meta"]["periEntryId"].clone();
        for id in &archived {
            assert_ne!(
                entry,
                Value::String(format!("{sid}:{}", id.as_uuid())),
                "归档条目不得出现在协商回放里"
            );
        }
    }
    assert_eq!(fixture.spy.full_reads(), 0, "协商路径不读全量 canonical");
    assert_eq!(
        fixture.spy.segment_calls().len(),
        0,
        "协商路径不走向前段读取"
    );
}

/// 本用例内的**第二实现**：按 ACP 投影契约从 canonical payload 推导期望 wire 事实序列。///
/// 事实格式：`text|{messageId}|{正文}` / `tool|{callId}` / `tool_update|{callId}`。
fn expected_wire_facts(payloads: &[PersistedPayload]) -> Vec<String> {
    let mut facts = Vec::new();
    for payload in payloads {
        let Some(message) = payload.as_message() else {
            continue;
        };
        let id = message.id().as_uuid().to_string();
        match message {
            BaseMessage::Human { .. } => {
                facts.push(format!("text|{id}|{}", message.content()));
            }
            BaseMessage::Ai { .. } => {
                let mut emitted: Vec<String> = Vec::new();
                if let peri_acp_types::messages::MessageContent::Blocks(blocks) =
                    message.message_content()
                {
                    for block in blocks {
                        match block {
                            ContentBlock::Reasoning { text, .. } => {
                                facts.push(format!("text|{id}|{text}"))
                            }
                            ContentBlock::Text { text } => facts.push(format!("text|{id}|{text}")),
                            ContentBlock::ToolUse { id: call, .. } => {
                                emitted.push(call.clone());
                                facts.push(format!("tool|{call}"));
                            }
                            _ => {}
                        }
                    }
                } else {
                    facts.push(format!("text|{id}|{}", message.content()));
                }
                for call in message.tool_calls() {
                    if !emitted.contains(&call.id) {
                        facts.push(format!("tool|{}", call.id));
                    }
                }
            }
            BaseMessage::Tool { tool_call_id, .. } => {
                facts.push(format!("tool_update|{tool_call_id}"));
            }
            // System：`project_message_updates` 明确返回空投影。
            _ => {}
        }
    }
    facts
}

/// 从 wire 上的 `session/update` 通知提取同一格式的事实序列（顺序即投递顺序）。
fn observed_wire_facts(fixture: &ReplayFixture) -> Vec<String> {
    let mut facts = Vec::new();
    for params in fixture.session_updates() {
        let update = &params["update"];
        match update["sessionUpdate"].as_str() {
            Some("user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk") => {
                let id = update["messageId"]
                    .as_str()
                    .expect("回放 chunk 必须携带 messageId");
                let text = update["content"]["text"].as_str().unwrap_or_default();
                facts.push(format!("text|{id}|{text}"));
            }
            Some("tool_call") => facts.push(format!(
                "tool|{}",
                update["toolCallId"].as_str().expect("tool_call 必须带身份")
            )),
            Some("tool_call_update") => facts.push(format!(
                "tool_update|{}",
                update["toolCallId"]
                    .as_str()
                    .expect("tool_call_update 必须带身份")
            )),
            _ => {}
        }
    }
    facts
}
