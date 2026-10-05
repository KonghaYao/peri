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

    assert_eq!(copied_payloads.len(), source_payloads.len());
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
