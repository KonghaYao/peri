//! fork 行为回归：一致 source 快照 → 纯 ID 映射 → 一次门面保存。
//!
//! 存储补偿（复制失败删新 thread）在新路径上不存在：目标快照由门面一次保存，
//! 失败不留下已发布的目标身份。这里用真实 SQLite 门面验证 ID/flags 独立性与
//! 领域拒绝条件，不用 mock 存储自洽推导。

use std::sync::Arc;

use peri_acp_types::messages::{BaseMessage, ToolCallRequest};
use peri_acp_types::projection::{
    MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResourceError,
    SessionResourceErrorKind, SessionResources,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::{ScopedThreadQuery, SessionBinding, ThreadScope};

use super::{fork_bound_session, load_fork_source};

const SOURCE: &str = "source";

/// 本文件的用例只读三条短条目：页预算够装下它们即可。取值刻意不引用
/// `host::history_page::PAGE_BYTE_BUDGET`（那是协议面默认值，与本用例要判的
/// 「父收缩是否改动子页内容」无关）。
const FORK_TEST_PAGE_BYTE_BUDGET: usize = 64 * 1024;

async fn open_facade(tmp: &tempfile::TempDir) -> Arc<dyn SessionResources> {
    peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
        .await
        .unwrap()
}

/// 建立一条已绑定、已持久化 frozen 的 source 会话（与生产 new 同一条路径）。
///
/// 保存源会话的完整不可变事实，供 fork 回归使用。
async fn create_source(resources: &Arc<dyn SessionResources>, id: &str, cwd: &str) {
    let workspace = resources
        .resolve_workspace(std::path::Path::new(cwd))
        .await
        .unwrap();
    resources
        .create_session(&NewSession {
            thread_id: id.to_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(format!(r#"{{"v":1,"id":"{id}"}}"#)),
        })
        .await
        .unwrap();
}

async fn session_ids(resources: &Arc<dyn SessionResources>) -> Vec<String> {
    resources
        .list_sessions(&ScopedThreadQuery {
            scope: ThreadScope::All,
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.thread.id)
        .collect()
}

#[tokio::test]
async fn forked_payloads_have_independent_ids_flags_and_compaction_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let resources = open_facade(&temp).await;
    let cwd = temp.path().to_string_lossy().into_owned();
    create_source(&resources, SOURCE, &cwd).await;
    let workspace = resources.resolve_workspace(temp.path()).await.unwrap();
    let source_id = SOURCE.to_owned();

    let source_messages = [
        BaseMessage::human("source question"),
        BaseMessage::tool_result("read-1", "source tool output"),
    ];
    let source_payloads = source_messages
        .iter()
        .cloned()
        .map(PersistedPayload::Message)
        .collect::<Vec<_>>();
    resources
        .append_history(&source_id, &source_payloads)
        .await
        .unwrap();
    let source_tool_id = source_messages[1].id();
    resources
        .apply_message_projections(
            &source_id,
            &[(
                source_tool_id,
                MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: Some(MessageProjectionDirective {
                        policy_version: peri_agent::agent::compact_v2::PROJECTION_POLICY_VERSION,
                        entries: vec![ProjectionActionEntry {
                            message_id: source_tool_id,
                            target: ProjectionTarget::Message,
                            action: ProjectionAction::CompactToolResult {
                                keep_head: 8,
                                keep_tail: 8,
                                preserve_recovery_handle: true,
                            },
                        }],
                    }),
                },
            )],
        )
        .await
        .unwrap();

    let source = load_fork_source(&resources, SOURCE).await.unwrap();
    let (fork_id, copied_payloads) = fork_bound_session(
        &resources,
        &source,
        &workspace,
        "2026-09-26T00:00:00Z".to_owned(),
    )
    .await
    .unwrap();

    assert_eq!(
        copied_payloads.len(),
        source_payloads.len(),
        "source 无已排除条目：发布集合与复制结果一致"
    );
    assert!(source_payloads
        .iter()
        .zip(&copied_payloads)
        .all(|(source, copied)| source.id() != copied.id()));
    assert_eq!(
        copied_payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        source_messages
            .iter()
            .map(BaseMessage::content)
            .collect::<Vec<_>>()
    );

    // 一次一致快照读回的目标历史与复制结果逐条一致（顺序与 ID 都不漂移）。
    let stored = resources.load_session_snapshot(&fork_id).await.unwrap();
    assert_eq!(
        stored
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        copied_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    let copied_tool_id = copied_payloads[1].id();
    let copied_tool_flags = &stored.flags[&copied_tool_id];
    assert!(copied_tool_flags.truncated);
    assert_eq!(
        copied_tool_flags.projection.as_ref().unwrap().entries[0].message_id,
        copied_tool_id,
        "fork 后 projection directive 必须引用新的消息 ID"
    );

    resources
        .apply_compaction(
            &fork_id,
            &CompactionChange {
                flag_updates: copied_payloads
                    .iter()
                    .map(|payload| {
                        (
                            payload.id(),
                            MessageFlags {
                                excluded: true,
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
                appended_messages: vec![BaseMessage::human("fork summary")],
            },
        )
        .await
        .expect("fork history 必须由新 thread 独立拥有并可提交 Full lifecycle");
    let source_snapshot = resources.load_session_snapshot(&source_id).await.unwrap();
    assert!(source_snapshot.flags[&source_tool_id].truncated);
    assert!(!source_snapshot.flags[&source_tool_id].excluded);
}

#[tokio::test]
async fn fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage() {
    let temp = tempfile::tempdir().unwrap();
    let resources = open_facade(&temp).await;
    let cwd = temp.path().to_string_lossy().into_owned();
    create_source(&resources, SOURCE, &cwd).await;
    let workspace = resources.resolve_workspace(temp.path()).await.unwrap();
    let source_id = SOURCE.to_owned();

    // Full Compact 后的 source：两条原文被排除，摘要成为唯一有效条目。
    let released = [
        BaseMessage::human("released question"),
        BaseMessage::ai("released answer"),
    ]
    .iter()
    .cloned()
    .map(PersistedPayload::Message)
    .collect::<Vec<_>>();
    resources
        .append_history(&source_id, &released)
        .await
        .unwrap();
    resources
        .apply_compaction(
            &source_id,
            &CompactionChange {
                flag_updates: released
                    .iter()
                    .map(|payload| {
                        (
                            payload.id(),
                            MessageFlags {
                                excluded: true,
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
                appended_messages: vec![BaseMessage::ai("source summary")],
            },
        )
        .await
        .unwrap();

    let source = load_fork_source(&resources, SOURCE).await.unwrap();
    let (fork_id, published) = fork_bound_session(
        &resources,
        &source,
        &workspace,
        "2026-10-13T00:00:00Z".to_owned(),
    )
    .await
    .unwrap();

    assert_eq!(published.len(), 1, "发布集合只含有效条目");
    assert_eq!(
        published[0].as_message().unwrap().content(),
        "source summary",
        "发布集合必须是 Full Compact 后的有效视图"
    );
    // 完整 canonical 历史（含已排除原文）随复制进目标存储：只有内存发布面收缩。
    let stored = resources.load_session_history(&fork_id).await.unwrap();
    assert_eq!(stored.len(), 3);
    assert!(stored.iter().any(|payload| payload
        .as_message()
        .is_some_and(|message| message.content() == "released question")));
    // 目标有效视图与发布集合逐条一致（同一份排除事实）。
    let active = resources.load_active_snapshot(&fork_id).await.unwrap();
    assert_eq!(
        active
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        published
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn fork_rejects_source_with_incomplete_tool_calls() {
    let temp = tempfile::tempdir().unwrap();
    let resources = open_facade(&temp).await;
    let cwd = temp.path().to_string_lossy().into_owned();
    create_source(&resources, SOURCE, &cwd).await;
    let source_id = SOURCE.to_owned();

    // 未闭合的工具调用：没有配对的 ToolResult，不能复制进独立可执行的历史。
    let open_call = BaseMessage::ai_with_tool_calls(
        "calling read",
        vec![ToolCallRequest::new(
            "call-1",
            "read",
            serde_json::json!({}),
        )],
    );
    resources
        .append_history(&source_id, &[PersistedPayload::Message(open_call)])
        .await
        .unwrap();

    let error = match load_fork_source(&resources, SOURCE).await {
        Ok(_) => panic!("未闭合工具调用必须被拒绝"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("incomplete tool calls"),
        "未闭合工具调用必须被领域侧拒绝，实际：{error}"
    );
    // 拒绝发生在任何写入之前：没有因此产生新的目标身份。
    assert_eq!(session_ids(&resources).await, vec![source_id]);

    // 缺失 source 同样在读取阶段失败（领域错误分类保留），不产生目标行。
    let missing = match load_fork_source(&resources, "no-such-source").await {
        Ok(_) => panic!("缺失 source 必须失败"),
        Err(error) => error,
    };
    let missing = missing
        .downcast::<SessionResourceError>()
        .expect("门面错误分类必须保留到调用方");
    assert!(matches!(missing.kind(), SessionResourceErrorKind::NotFound));
    assert_eq!(session_ids(&resources).await, vec![SOURCE.to_owned()]);
}

/// A14 第三句：**既有 fork 之后，父会话再走一次 Full**——子快照与子分页内容都不变。
///
/// 与 `forked_payloads_have_independent_ids_flags_and_compaction_lifecycle` 方向相反：
/// 那条断言「子 Full 不改父」，本用例断言「父收缩不得回流到子」。两条读取都是子侧对外的
/// 事实源（[`SessionResources::load_session_snapshot`] = 子快照、
/// [`SessionResources::load_history_page`] = 子分页内容），断言不重新推导存储事实。
#[tokio::test]
async fn a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact() {
    let temp = tempfile::tempdir().unwrap();
    let resources = open_facade(&temp).await;
    let cwd = temp.path().to_string_lossy().into_owned();
    create_source(&resources, SOURCE, &cwd).await;
    let workspace = resources.resolve_workspace(temp.path()).await.unwrap();

    // 父会话自有区：三条都可展示（human / tool_result / ai），供子侧一次读尽。
    let parent_messages = [
        BaseMessage::human("父会话问题"),
        BaseMessage::tool_result("read-a14", "父会话的工具输出"),
        BaseMessage::ai("父会话回答"),
    ];
    resources
        .append_history(
            &SOURCE.to_owned(),
            &parent_messages
                .iter()
                .cloned()
                .map(PersistedPayload::Message)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();

    let source = load_fork_source(&resources, SOURCE).await.unwrap();
    let (fork_id, _published) = fork_bound_session(
        &resources,
        &source,
        &workspace,
        "2026-10-13T00:00:00Z".to_owned(),
    )
    .await
    .unwrap();

    // 父收缩之前的子侧事实：快照（身份 + 正文 + flags）与首页（条目身份 + 顺序键 + 游标链）。
    let before_snapshot = resources.load_session_snapshot(&fork_id).await.unwrap();
    let before_page = resources
        .load_history_page(&fork_id, None, 50, FORK_TEST_PAGE_BYTE_BUDGET)
        .await
        .unwrap();
    assert_eq!(
        before_snapshot.payloads.len(),
        parent_messages.len(),
        "先决条件：fork 复制出父会话的全部自有条目"
    );
    assert_eq!(
        before_page.items.len(),
        parent_messages.len(),
        "先决条件：子首页一次读尽（否则下面的「等价」可能是两边都空）"
    );
    assert!(
        before_page.next_cursor.is_none(),
        "先决条件：子历史已到起点，没有更早页"
    );
    // 「不变」必须钉在**绝对值**上：先把收缩前的子侧事实对齐父会话的三条原文，
    // 否则「前后一致」可能只是两边都错成同一个样子。
    let expected_contents: Vec<String> = parent_messages.iter().map(BaseMessage::content).collect();
    assert_eq!(
        before_snapshot
            .payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        expected_contents,
        "先决条件：子快照就是父会话复制来的这三条"
    );
    assert_eq!(
        before_page
            .items
            .iter()
            .filter_map(|item| item.payload.as_message())
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        expected_contents,
        "先决条件：子首页内容就是这三条（父收缩之前）"
    );

    // 父会话的后续 Full：自有条目全部归档 + 追加摘要（与 `PageFixture::archive` 同形状）。
    let parent_ids: Vec<_> = resources
        .load_session_snapshot(&SOURCE.to_owned())
        .await
        .unwrap()
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();
    resources
        .apply_compaction(
            &SOURCE.to_owned(),
            &CompactionChange {
                flag_updates: parent_ids
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
                appended_messages: vec![BaseMessage::ai("父会话摘要")],
            },
        )
        .await
        .expect("父会话的第二次 Full 必须可提交");

    // 子快照：条目身份、正文、flags 三项逐条等价。
    let after_snapshot = resources.load_session_snapshot(&fork_id).await.unwrap();
    assert_eq!(
        after_snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        before_snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        "父收缩不得改变子快照的条目身份"
    );
    assert_eq!(
        after_snapshot
            .payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        before_snapshot
            .payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        "父收缩不得改变子快照的正文（父会话新摘要不得进入子快照）"
    );
    assert_eq!(
        after_snapshot.flags, before_snapshot.flags,
        "父收缩不得改变子条目的 flags"
    );

    // 子分页：同一批条目、同一顺序键、同一条游标链。
    let after_page = resources
        .load_history_page(&fork_id, None, 50, FORK_TEST_PAGE_BYTE_BUDGET)
        .await
        .unwrap();
    assert_eq!(
        after_page
            .items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        before_page
            .items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        "父收缩不得改变子分页的条目身份"
    );
    assert_eq!(
        after_page
            .items
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>(),
        before_page
            .items
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>(),
        "父收缩不得改变子分页的顺序键"
    );
    assert_eq!(
        after_page
            .items
            .iter()
            .filter_map(|item| item.payload.as_message())
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        before_page
            .items
            .iter()
            .filter_map(|item| item.payload.as_message())
            .map(BaseMessage::content)
            .collect::<Vec<_>>(),
        "父会话新摘要不得出现在子分页里"
    );
    assert_eq!(
        after_page.next_cursor, before_page.next_cursor,
        "父收缩不得改变子浏览的游标链"
    );

    // 反向对照：这一轮收缩**确实**落在父会话上（否则上面的「不变」是空转）。
    let after_source = resources
        .load_session_snapshot(&SOURCE.to_owned())
        .await
        .unwrap();
    assert!(
        parent_ids.iter().all(|id| after_source
            .flags
            .get(id)
            .is_some_and(|flags| flags.excluded)),
        "父会话自身的全部自有条目必须已被这次 Full 排除"
    );
    assert!(
        after_source
            .payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .any(|message| message.content() == "父会话摘要"),
        "父会话必须已追加摘要"
    );
    assert!(
        after_snapshot.flags.values().all(|flags| !flags.excluded),
        "子条目不得被父会话的 Full 排除"
    );
}
