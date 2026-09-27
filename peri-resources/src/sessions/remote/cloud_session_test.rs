//! 显式云端会话实验（默认 `#[ignore]`）：真实写 → 重连读 → 复核 → 只清理本轮。
//!
//! 覆盖（每条都断言可观察结果）：
//!
//! | 实验 | 断言的事实 |
//! | --- | --- |
//! | 写—重连读 | 新建/fork/child 的事实在新连接（只读打开）上逐字段读回：meta、绑定、frozen 原文、payload 顺序、非默认 flags、继承区 |
//! | 列举与树 | children/tree 是父子关系事实；scoped 分页只给出带历史的会话，条目只带绑定事实不带本机根目录 |
//! | 只读与关闭 | 只读打开拒绝写入（`ReadOnlyStore`）；关闭后调用明确失败 |
//! | 拒绝与重放 | child 的 frozen 不是 root 已保存的原文 → `InvalidInput` 且不落行；同一内容重试不产生第二行；同 id 不同内容 → 冲突失败且不改动已有行 |
//!
//! 安全与清理：
//!
//! - 只操作**本轮 run 命名空间**下的行（`thread_id` 以 run 前缀开头；账本行按 run 标签
//!   匹配），结束时删除本轮行并**复核计数为 0**；不动任务 schema、不动其他轮次的数据。
//! - 只输出安全状态/计数/布尔事实，全部经 [`super::cloud_tests::SafeOut`] 校验；
//!   本文件不打印 URL、token、SQL 参数或 SDK 原始错误，失败只给领域类别名。
//! - 测试数据全部是本进程合成的短文本（不含真实历史、当前项目内容或凭证）。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_session_
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use super::cloud_tests::{
    check, failure, run_counts, session_input, unique_run_label, with_cleanup, CloudTarget,
};
use super::mutation::StoreAccess;
use crate::sessions::data::SessionDataPort;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, FrozenState, SessionMetaPatch,
    SessionResourceErrorKind,
};
use peri_acp_types::store::{InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workspace::{
    ProjectId, ScopedThreadQuery, SessionBinding, ThreadScope, WorkspaceId,
};

/// 实验一：写 → 重连（只读打开）读回全部事实 → 列举/树 → 只读拒绝 → 关闭。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_session_write_read_reconnect_round_trip() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-sess");
    let mut out = target.out();
    out.push(format!("run_prefix_len={}", run.len()));
    out.push("experiment=session_round_trip".to_owned());
    out.flush();
    let result = with_cleanup(&target, &run, || async {
        session_round_trip(&target, &run).await
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("round_trip=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn session_round_trip(target: &CloudTarget, run: &str) -> Result<(), String> {
    let writer = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let root = format!("{run}-root");
    let fork = format!("{run}-fork");
    let child = format!("{run}-child");
    let binding = SessionBinding {
        schema_version: 1,
        revision: 1,
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        cwd_relative_to_workspace: PathBuf::from("sub"),
    };
    let frozen = format!("{{\"frozen\":\"{run}\"}}");
    let created_at = chrono::Utc::now().to_rfc3339();

    // 新建：meta + 绑定 + frozen 一次落库。
    writer
        .save_new_session(&session_input(&root, &created_at, &binding, &frozen, None))
        .await
        .map_err(failure)?;
    // 定向 metadata：标题与状态。
    writer
        .update_meta(
            &root,
            &SessionMetaPatch {
                title: Some(Some("renamed".to_owned())),
                status: Some(AgentStatus::Done),
                cancel_policy: None,
                config: Some(Some("{\"k\":1}".to_owned())),
            },
        )
        .await
        .map_err(failure)?;

    // fork：两条 payload（领域重映射后的结果）+ 一条非默认 flag。
    let first = PersistedPayload::Message(BaseMessage::human("fork one"));
    let second = PersistedPayload::Message(BaseMessage::ai("fork two"));
    let mut flags = HashMap::new();
    flags.insert(
        first.id(),
        MessageFlags {
            truncated: true,
            excluded: false,
            projection: None,
        },
    );
    writer
        .save_fork(&ForkSnapshot {
            target: session_input(&fork, &created_at, &binding, &frozen, None),
            source_id: root.clone(),
            payloads: vec![first.clone(), second.clone()],
            flags,
        })
        .await
        .map_err(failure)?;

    // child：继承区 + 父子/根归属 + root 的 frozen 原文。
    let inherited_payload = PersistedPayload::Message(BaseMessage::human("inherited"));
    writer
        .save_child(&ChildSnapshot {
            target: session_input(&child, &created_at, &binding, &frozen, Some(&root)),
            parent_id: root.clone(),
            root_id: root.clone(),
            inherited: InheritedContext {
                payloads: vec![inherited_payload.clone()],
                flags: HashMap::new(),
            },
        })
        .await
        .map_err(failure)?;

    // 重连：新连接、只读打开，逐字段复核（不靠写路径的内存状态）。
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(failure)?;

    let root_meta = reader.load_meta(&root).await.map_err(failure)?;
    check(
        root_meta.title.as_deref() == Some("renamed"),
        "title patch is not visible after reconnect",
    )?;
    check(
        root_meta.agent_status == AgentStatus::Done,
        "status patch is not visible after reconnect",
    )?;
    check(
        root_meta.config.as_deref() == Some("{\"k\":1}"),
        "config patch is not visible after reconnect",
    )?;
    check(
        root_meta.message_count == 0,
        "session without history must report zero messages",
    )?;
    check(
        root_meta.cached_context.is_none(),
        "remote store must not invent a materialized cache",
    )?;

    let root_snapshot = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        root_snapshot.binding == BindingState::Bound(binding.clone()),
        "root binding is not the immutable binding that was written",
    )?;
    check(
        root_snapshot.frozen == FrozenState::Present(FrozenSnapshotBytes::new(frozen.clone())),
        "root frozen bytes are not returned verbatim",
    )?;
    check(
        root_snapshot.payloads.is_empty() && root_snapshot.flags.is_empty(),
        "root session must have no own history",
    )?;

    let fork_snapshot = reader.load_snapshot(&fork).await.map_err(failure)?;
    check(
        fork_snapshot.payloads.len() == 2
            && fork_snapshot.payloads[0].id() == first.id()
            && fork_snapshot.payloads[1].id() == second.id(),
        "fork payloads are not the mapped batch in order",
    )?;
    check(
        fork_snapshot.meta.message_count == 2,
        "fork derived message count is not the payload count",
    )?;
    check(
        fork_snapshot
            .flags
            .get(&first.id())
            .map(|flags| flags.truncated)
            == Some(true)
            && !fork_snapshot.flags.contains_key(&second.id()),
        "derived flags view is not the non-default set",
    )?;

    let child_snapshot = reader.load_snapshot(&child).await.map_err(failure)?;
    check(
        child_snapshot.inherited.payloads.len() == 1
            && child_snapshot.inherited.payloads[0].id() == inherited_payload.id(),
        "child inherited context is not the snapshot that was written",
    )?;
    check(
        child_snapshot.frozen == root_snapshot.frozen,
        "child frozen must be the root's saved bytes",
    )?;
    check(
        child_snapshot.meta.parent_thread_id.as_deref() == Some(root.as_str()),
        "child parent relation is not persisted",
    )?;
    check(
        child_snapshot.binding == BindingState::Bound(binding.clone()),
        "child binding is not the inherited immutable binding",
    )?;

    let children = reader.list_children(&root).await.map_err(failure)?;
    check(
        children.len() == 1 && children[0].id == child,
        "children listing is not the direct child set",
    )?;
    let tree = reader.list_session_tree(&root).await.map_err(failure)?;
    let tree_ids: Vec<&str> = tree.iter().map(|meta| meta.id.as_str()).collect();
    check(
        tree.len() == 2 && tree_ids.contains(&root.as_str()) && tree_ids.contains(&child.as_str()),
        "session tree is not root plus descendants",
    )?;

    // scoped 分页：只有带历史的会话出现；条目只带绑定事实，不带本机解析出的根目录。
    let page = reader
        .list_sessions(&ScopedThreadQuery {
            scope: ThreadScope::Project(binding.project_id),
            cursor: None,
            limit: 50,
        })
        .await
        .map_err(failure)?;
    let page_ids: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| entry.thread.id.as_str())
        .collect();
    check(
        page_ids.contains(&fork.as_str())
            && !page_ids.contains(&root.as_str())
            && !page_ids.contains(&child.as_str()),
        "scoped listing must contain only sessions with history",
    )?;
    let entry = page
        .entries
        .iter()
        .find(|entry| entry.thread.id == fork)
        .ok_or("fork session is missing from its scoped listing")?;
    check(
        entry.binding.as_ref() == Some(&binding) && entry.workspace_root.is_none(),
        "listing entries must carry binding facts only",
    )?;

    // 逻辑上下文：继承区在前、自有 payload 在后。
    let fork_history = reader.load_session_history(&fork).await.map_err(failure)?;
    check(
        fork_history.len() == 2 && fork_history[0].id() == first.id(),
        "fork logical context is not its own payload batch",
    )?;
    let child_history = reader.load_session_history(&child).await.map_err(failure)?;
    check(
        child_history.len() == 1 && child_history[0].id() == inherited_payload.id(),
        "child logical context must lead with the inherited region",
    )?;

    // 只读打开不写：写路径在发请求前就拒绝。
    let refused = reader
        .save_new_session(&session_input(
            &format!("{run}-extra"),
            &created_at,
            &binding,
            &frozen,
            None,
        ))
        .await;
    check(
        matches!(&refused, Err(error) if matches!(error.kind(), SessionResourceErrorKind::ReadOnlyStore)),
        "read-only open must refuse writes",
    )?;

    // 关闭后不再接受调用。
    reader.close().await.map_err(failure)?;
    check(
        reader.load_meta(&root).await.is_err(),
        "closed port must fail loudly",
    )?;
    writer.close().await.map_err(failure)?;
    Ok(())
}

/// 实验二：拒绝与重复调用都不产生第二行，也不改动已有行。
///
/// 注意与 C-03 第二批的差别：操作身份不再由内容派生，因此「同内容再来一次」是**新的
/// 领域调用**（照实撞主键），而不是被当成重放静默跳过——重放语义只覆盖同一次操作。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_session_refusals_and_replay_leave_facts_unchanged() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-sess-neg");
    let result = with_cleanup(&target, &run, || async {
        session_refusals(&target, &run).await
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("refusals_and_replay=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn session_refusals(target: &CloudTarget, run: &str) -> Result<(), String> {
    let adapter = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let root = format!("{run}-root");
    let child = format!("{run}-child");
    let binding = SessionBinding {
        schema_version: 1,
        revision: 1,
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        cwd_relative_to_workspace: PathBuf::from("sub"),
    };
    let frozen = format!("{{\"frozen\":\"{run}\"}}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let first_input = session_input(&root, &created_at, &binding, &frozen, None);

    adapter
        .save_new_session(&first_input)
        .await
        .map_err(failure)?;

    // 同 id、不同内容：必须是冲突失败（不能冒充「已应用」），也不得改动已有行。
    let conflicting = session_input(
        &root,
        &created_at,
        &binding,
        &format!("{{\"frozen\":\"{run}-other\"}}"),
        None,
    );
    let conflict = adapter.save_new_session(&conflicting).await;
    check(
        matches!(&conflict, Err(error) if matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. })),
        "a re-create with different content must fail",
    )?;

    // 同一内容再保存一次：**这是新的领域调用**，不是历史重放（操作 id 每次唯一）。
    // 它照实撞上会话主键 → 确定未生效（InvalidInput），且不改动已有行、不产生第二行。
    // 「同一次请求的重试」由 adapter 内部复用同一操作 id 承担（v10 撤销本机日志后不再有
    // 跨进程的原 id 取回路径），不靠调用方传令牌。
    let repeated = adapter.save_new_session(&first_input).await;
    check(
        matches!(&repeated, Err(error) if matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. })),
        "re-saving the same input is a new operation and must fail on the existing row",
    )?;

    // child 的 frozen 不是 root 已保存的原文：拒绝，且不落行。
    let refused_child = adapter
        .save_child(&ChildSnapshot {
            target: session_input(
                &child,
                &created_at,
                &binding,
                &format!("{{\"frozen\":\"{run}-foreign\"}}"),
                Some(&root),
            ),
            parent_id: root.clone(),
            root_id: root.clone(),
            inherited: InheritedContext::default(),
        })
        .await;
    check(
        matches!(&refused_child, Err(error) if matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. })),
        "child frozen that is not the root's saved bytes must be refused",
    )?;
    adapter.close().await.map_err(failure)?;

    // 重连复核：一行 root（内容仍是第一次写入的事实）、没有 child 行。
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(failure)?;
    check(
        reader.exists(&root).await.map_err(failure)?,
        "root session must exist after replay",
    )?;
    check(
        !reader.exists(&child).await.map_err(failure)?,
        "refused child must not exist",
    )?;
    let snapshot = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        snapshot.frozen == FrozenState::Present(FrozenSnapshotBytes::new(frozen.clone())),
        "a refused re-create must not overwrite the saved frozen bytes",
    )?;
    reader.close().await.map_err(failure)?;

    // 独立只读连接复核行数：本轮只有 root 一行会话、没有历史行。
    let store = target.store(StoreAccess::ReadOnly).await.map_err(failure)?;
    let counts = run_counts(&store, run).await?;
    store.close().await.map_err(failure)?;
    check(
        counts.sessions == 1 && counts.messages == 0,
        "refusals and replay must leave exactly the first session row",
    )?;
    let mut out = target.out();
    out.push(format!(
        "run_rows sessions={} messages={}",
        counts.sessions, counts.messages
    ));
    out.flush();
    Ok(())
}
