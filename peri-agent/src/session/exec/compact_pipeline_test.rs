//! `/compact` 身份校验的继承区有效投影回归测试。
//!
//! 回归：子会话的冻结继承区同时保留 excluded 原文与 flags，普通 turn 交给调用方的
//! 有效集已按 flags 过滤；持久侧身份序列若仍包含这些条目，`/compact` 会误报
//! `compact persistence context mismatch`。夹具使用真 SQLite 门面：继承区、flags
//! 与提交后的历史都必须来自真实实现，不用替身推导。
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use peri_acp_types::command::{CommandContext, DependencyBag, FeedbackLevel};
use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::store::{InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::SessionBinding;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    StopReason,
};
use peri_resources::sessions::SessionResourcesImpl;
use tokio_util::sync::CancellationToken;

use crate::agent::compact_v2::CompactConfig;
use crate::messages::BaseMessage;
use crate::session::exec::compact_pipeline::execute_compact;

struct Fixture {
    resources: Arc<dyn SessionResources>,
    child_id: String,
    cwd: String,
    /// 继承区里仍有效的条目（普通 turn 有效集包含它们）。
    inherited_active: Vec<BaseMessage>,
    /// 继承区里已被父会话归档的条目（有效集不包含，但正文与 flags 仍冻结在子会话）。
    inherited_excluded: BaseMessage,
    /// 子会话 resume 后续聊产生的自有消息。
    own: Vec<BaseMessage>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("compact-inherited.db");
        let resources: Arc<dyn SessionResources> =
            Arc::new(SessionResourcesImpl::open(&db_path).await.unwrap());
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        let cwd = workspace.cwd.to_string_lossy().into_owned();
        let frozen = FrozenSnapshotBytes::new("{\"version\":1,\"root\":true}");
        let parent_id = uuid::Uuid::now_v7().to_string();
        resources
            .create_session(&NewSession {
                thread_id: parent_id.clone(),
                created_at: "2026-10-18T00:00:00Z".into(),
                meta: NewSessionMeta {
                    title: Some("parent".into()),
                    cwd: cwd.clone(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: frozen.clone(),
            })
            .await
            .unwrap();

        let inherited_active = vec![
            BaseMessage::human("inherited active task"),
            BaseMessage::human("inherited active followup"),
        ];
        let inherited_excluded = BaseMessage::ai("inherited archived answer");
        // 父会话原文：中间一条已被 Full 归档。子会话继承时正文与 flags 一起冻结。
        resources
            .append_history(
                &parent_id,
                &[
                    PersistedPayload::Message(inherited_active[0].clone()),
                    PersistedPayload::Message(inherited_excluded.clone()),
                    PersistedPayload::Message(inherited_active[1].clone()),
                ],
            )
            .await
            .unwrap();
        resources
            .apply_message_projections(
                &parent_id,
                &[(
                    inherited_excluded.id(),
                    MessageFlags {
                        excluded: true,
                        ..Default::default()
                    },
                )],
            )
            .await
            .unwrap();

        let child_id = uuid::Uuid::now_v7().to_string();
        resources
            .save_child(&ChildSnapshot {
                target: NewSession {
                    thread_id: child_id.clone(),
                    created_at: "2026-10-18T00:00:01Z".into(),
                    meta: NewSessionMeta {
                        title: Some("child".into()),
                        cwd: cwd.clone(),
                        parent_thread_id: Some(parent_id.clone()),
                        hidden: false,
                        cancel_policy: CancelPolicy::default(),
                        snapshot_at_message_id: None,
                    },
                    binding: SessionBinding::from_workspace(&workspace),
                    frozen: frozen.clone(),
                },
                parent_id: parent_id.clone(),
                root_id: parent_id,
                inherited: InheritedContext {
                    payloads: vec![
                        PersistedPayload::Message(inherited_active[0].clone()),
                        PersistedPayload::Message(inherited_excluded.clone()),
                        PersistedPayload::Message(inherited_active[1].clone()),
                    ],
                    flags: std::collections::HashMap::from([(
                        inherited_excluded.id(),
                        MessageFlags {
                            excluded: true,
                            ..Default::default()
                        },
                    )]),
                },
            })
            .await
            .unwrap();

        let own = vec![
            BaseMessage::human("child resumed request"),
            BaseMessage::ai("child resumed answer"),
        ];
        resources
            .append_history(
                &child_id,
                &own.iter()
                    .cloned()
                    .map(PersistedPayload::Message)
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();

        Self {
            resources,
            child_id,
            cwd,
            inherited_active,
            inherited_excluded,
            own,
            _directory: directory,
        }
    }

    /// 普通 turn 交给 `/compact` 的有效集合：继承区 active + own active。
    fn effective_history(&self) -> Vec<BaseMessage> {
        let mut history = self.inherited_active.clone();
        history.extend(self.own.clone());
        history
    }

    async fn run(&self, history: Vec<BaseMessage>) -> peri_acp_types::command::CommandResult {
        let sink = Arc::new(RecordingSink::default());
        let mut ctx = CommandContext::new(
            self.child_id.clone(),
            history,
            self.cwd.clone(),
            sink,
            CancellationToken::new(),
            DependencyBag::new(),
        );
        ctx.compact_config = CompactConfig::default();
        ctx.auxiliary_model = Some(Arc::new(SummaryModel));
        ctx.session_resources = Some(self.resources.clone());
        ctx.thread_id = Some(self.child_id.clone());
        execute_compact(ctx).await
    }
}

struct SummaryModel;

#[async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("摘要必须走 complete")
    }

    async fn complete(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelResponse> {
        ModelResponse::new(
            ModelMessage::assistant_text("<summary>child inherited summary</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )
    }
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<ExecutorEvent>>);

#[async_trait]
impl EventSink for RecordingSink {
    async fn push_event(&self, _: &str, event: &ExecutorEvent, _: u32) {
        self.0.lock().push(event.clone());
    }

    async fn push_done(&self, _: &str, _: &str, _: Option<&str>) {}
}

fn assert_context_mismatch(result: &peri_acp_types::command::CommandResult) {
    let feedback = result.feedback.as_ref().expect("必须返回结构化反馈");
    assert_eq!(feedback.level, FeedbackLevel::Error);
    assert!(
        feedback.message.contains("context mismatch"),
        "{}",
        feedback.message
    );
}

/// [回归] 子会话冻结继承区带 excluded 条目时，`/compact` 按有效投影比较并通过。
#[tokio::test]
async fn test_compact_accepts_excluded_inherited_in_effective_projection() {
    let fixture = Fixture::open().await;
    let result = fixture.run(fixture.effective_history()).await;

    let feedback = result.feedback.as_ref().expect("必须返回结构化反馈");
    assert_eq!(
        feedback.level,
        FeedbackLevel::Info,
        "合法继承上下文不得误报：{}",
        feedback.message
    );
    assert_eq!(feedback.message, "已压缩 2 条消息");

    let summary_id = result.messages[0].id();
    assert!(matches!(
        result.messages.first(),
        Some(BaseMessage::Human { .. })
    ));
    assert!(result.messages[0]
        .content()
        .contains("child inherited summary"));
    let returned_ids: Vec<_> = result.messages.iter().map(BaseMessage::id).collect();
    assert_eq!(
        returned_ids,
        vec![
            summary_id,
            fixture.inherited_active[0].id(),
            fixture.inherited_active[1].id(),
        ],
        "命令输出 = 摘要 + 继承区有效条目，归档继承原文不得出现"
    );
    assert!(!returned_ids.contains(&fixture.inherited_excluded.id()));

    let snapshot = fixture
        .resources
        .load_session_snapshot(&fixture.child_id)
        .await
        .unwrap();
    // 继承区仍是一份冻结快照，既不裁正文也不复制成 own。
    let inherited_ids: Vec<_> = snapshot
        .inherited
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(
        inherited_ids,
        vec![
            fixture.inherited_active[0].id(),
            fixture.inherited_excluded.id(),
            fixture.inherited_active[1].id(),
        ]
    );
    let own_ids: Vec<_> = snapshot.payloads.iter().map(PersistedPayload::id).collect();
    assert_eq!(
        own_ids,
        vec![fixture.own[0].id(), fixture.own[1].id(), summary_id,],
        "own 区只允许原续聊消息与摘要，不得出现继承区副本"
    );
    assert!(!own_ids.contains(&fixture.inherited_excluded.id()));
    for archived in &fixture.own {
        let flags = snapshot
            .flags
            .get(&archived.id())
            .expect("own 条目应带归档标记");
        assert!(flags.excluded);
        assert_eq!(flags.superseded_by(), Some(summary_id));
    }

    // 完整逻辑上下文（继承区在前、own 在后）：每条只出现一次，归档继承原文没有重复。
    let history_ids: Vec<_> = fixture
        .resources
        .load_session_history(&fixture.child_id)
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(
        history_ids,
        vec![
            fixture.inherited_active[0].id(),
            fixture.inherited_excluded.id(),
            fixture.inherited_active[1].id(),
            fixture.own[0].id(),
            fixture.own[1].id(),
            summary_id,
        ]
    );
    assert_eq!(
        history_ids
            .iter()
            .filter(|id| **id == fixture.inherited_excluded.id())
            .count(),
        1
    );

    // 活跃视图：归档的继承条目仍只在继承区并保持 excluded；own 只剩摘要。
    let active = fixture
        .resources
        .load_active_snapshot(&fixture.child_id)
        .await
        .unwrap();
    assert_eq!(
        active
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![summary_id]
    );
    assert!(active
        .inherited
        .flags
        .get(&fixture.inherited_excluded.id())
        .is_some_and(|flags| flags.excluded));
}

/// [回归] 旧调用方仍带回完整历史时，多出的继承区 id 必须被同一快照的
/// excluded flags 证明，而不是靠放宽比较放行。
#[tokio::test]
async fn test_compact_accepts_full_history_with_known_excluded_inherited() {
    let fixture = Fixture::open().await;
    let mut full_history = vec![
        fixture.inherited_active[0].clone(),
        fixture.inherited_excluded.clone(),
        fixture.inherited_active[1].clone(),
    ];
    full_history.extend(fixture.own.clone());
    let result = fixture.run(full_history).await;

    let feedback = result.feedback.as_ref().expect("必须返回结构化反馈");
    assert_eq!(feedback.level, FeedbackLevel::Info, "{}", feedback.message);
    assert!(!result
        .messages
        .iter()
        .any(|message| message.id() == fixture.inherited_excluded.id()));
}

/// [回归] 真实不一致（顺序、有效位置、空集合）仍必须拒绝，且不产生提交。
#[tokio::test]
async fn test_compact_rejects_tampered_effective_sequence() {
    let fixture = Fixture::open().await;

    let mut reordered = fixture.effective_history();
    reordered.swap(0, 1);
    assert_context_mismatch(&fixture.run(reordered).await);

    let mut replaced = fixture.effective_history();
    replaced[2] = BaseMessage::human("injected replacement");
    assert_context_mismatch(&fixture.run(replaced).await);

    // 空有效集不得被当作「没有差异」放行。
    assert_context_mismatch(&fixture.run(Vec::new()).await);

    let snapshot = fixture
        .resources
        .load_session_snapshot(&fixture.child_id)
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![fixture.own[0].id(), fixture.own[1].id()],
        "拒绝路径不得追加摘要或改写历史"
    );
    assert!(snapshot.flags.values().all(|flags| !flags.excluded));
}
