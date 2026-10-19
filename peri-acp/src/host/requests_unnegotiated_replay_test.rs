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
            connection_id: "unnegotiated-test".into(),
            history_pagination,
            history_window_v1: false,
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

// 本模块的用例按职责拆到三个子模块（正文逐行原样搬移，语义零改动）：交付内容与 wire 形状、
// 有界读取与失败关闭/回退、S9 独立复核。夹具与替身留在本文件，子模块经 `use super::*` 复用；
// 拆分依据与对应关系见 `.recon/S13-size-acp-tests.md`。

#[path = "requests_unnegotiated_replay_delivery_test.rs"]
mod delivery_tests;

#[path = "requests_unnegotiated_replay_fail_closed_test.rs"]
mod fail_closed_tests;

#[path = "requests_unnegotiated_replay_verification_test.rs"]
mod verification_tests;
