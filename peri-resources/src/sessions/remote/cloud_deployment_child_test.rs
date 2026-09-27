//! 显式云端端到端的**子进程阶段**（父测试：[`super::cloud_deployment_tests`]）。
//!
//! 三个测试各自代表一个进程阶段：写入、冷恢复、显式只读。没有父测试放进来的标记变量时
//! 它们直接返回，因此只有被父测试拉起时才工作；断言与安全规则见父模块文档。
//!
//! - 写入：真实部署入口 → 创建 → 追加/排空 → compact → fork → child → 标题 A→B→A → close；
//! - 冷恢复：同一 HOME 的**新进程** → 未决收敛 → 解除 ordinary dirty → rewind → 删除；
//! - 只读：`fresh`（全新 HOME，本机无库）与 `registered`（沿用写入期 HOME）两种本机状态。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    AccessMode, DataCapabilities, ExecutionAvailability, ForkSnapshot, FrozenSnapshotBytes,
    FrozenState, NewSession, NewSessionMeta, PersistenceRecovery, RewindBoundary, SessionMetaPatch,
    SessionResourceErrorKind, SessionResources,
};
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_acp_types::store::{CompactionChange, InheritedContext, PersistedPayload};
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ResetDirtyRequest, SessionBinding, SESSION_BINDING_VERSION};

use super::cloud_deployment_tests::{
    CHILD_SUFFIX, FORK_SUFFIX, HOME_ENV, READ_ONLY_ENV, ROOT_SUFFIX, RUN_ENV, WORKSPACE_ENV,
};
use super::cloud_tests::{
    check, failure, load_env, remote_error_class, required_credential_keys, required_credentials,
    CloudTarget,
};
use crate::sessions::SessionResourcesImpl;

/// 子进程启动上下文；没有标记变量时返回 `None`（该测试只在被父测试拉起时工作）。
struct ChildContext {
    home: PathBuf,
    workspace: PathBuf,
    run: String,
    env: BTreeMap<String, String>,
}

fn child_context() -> Option<ChildContext> {
    let home = std::env::var_os(HOME_ENV)?;
    let workspace = std::env::var_os(WORKSPACE_ENV)?;
    let run = std::env::var(RUN_ENV).ok()?;
    Some(ChildContext {
        home: PathBuf::from(home),
        workspace: PathBuf::from(workspace),
        run,
        env: load_env(),
    })
}

impl ChildContext {
    /// 部署参数：locator 原文 + 显式引擎名 + 凭证**来源**（环境变量名）。
    ///
    /// 凭证值只在本进程内注入同名变量：部署参数按设计只接受来源名，门面再按名取值。
    fn deployment(&self, access: AccessMode) -> Result<SessionStoreDeployment, String> {
        let (_, token_key) = required_credential_keys();
        let (url, token) = required_credentials(&self.env);
        std::env::set_var(&token_key, &token);
        let engine = self
            .endpoint()
            .map_err(|error| error.to_string())?
            .engine()
            .as_str()
            .to_owned();
        Ok(SessionStoreDeployment::from_locator(url)
            .with_engine(engine)
            .with_credential_env(token_key)
            .with_access(access))
    }

    /// 端点（与门面解析 locator 用的是同一段纯解析，因此引擎名与摘要一致）。
    fn endpoint(&self) -> Result<super::RemoteEndpoint, super::EndpointError> {
        let (url, _) = required_credentials(&self.env);
        super::RemoteEndpoint::parse(&url, None)
    }

    fn path(&self, suffix: &str) -> String {
        format!("{}-{suffix}", self.run)
    }

    fn thread(&self, suffix: &str) -> ThreadId {
        self.path(suffix)
    }
}
/// 页面上的只读检查结果：只带计数与布尔，不带 locator 或内容。
fn report(out: &mut super::cloud_tests::SafeOut, lines: &[String]) {
    for line in lines {
        out.push(line.clone());
    }
}

// ─── 子进程 A：写入 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn cloud_deployment_child_writes_the_synthetic_tree() {
    let Some(context) = child_context() else {
        return;
    };
    let target = CloudTarget::load();
    let mut out = target.out();
    let lines = write_flow(&context, &target)
        .await
        .unwrap_or_else(|message| panic!("{message}"));
    report(&mut out, &lines);
    out.push("write_phase=ok".to_owned());
    out.flush();
}

async fn write_flow(context: &ChildContext, target: &CloudTarget) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    // 配置即用：配了哪个 store 就直接用，打开之前不需要任何本机登记（v10 撤销了登记链）。
    let facade = open_facade(context, AccessMode::ReadWrite, target).await?;
    let workspace = facade
        .resolve_workspace(&context.workspace)
        .await
        .map_err(failure)?;
    let root = context.thread(ROOT_SUFFIX);

    // ① 远端 durable 保存 + 本机执行准入。
    let lease = facade
        .create_session(&session_input(context, &root, &workspace, None))
        .await
        .map_err(failure)?;

    // ② 追加 + 排空（append/flush）：排空是「已排队的写入结束」的有界等待，不是成功声明。
    let first = PersistedPayload::Message(BaseMessage::human("synthetic question"));
    let second = PersistedPayload::Message(BaseMessage::ai("synthetic answer"));
    facade
        .append_history(&root, &[first.clone(), second.clone()])
        .await
        .map_err(failure)?;
    facade.drain_persistence(&root).await.map_err(failure)?;
    lines.push(format!(
        "append_rows={}",
        facade
            .load_session_history(&root)
            .await
            .map_err(failure)?
            .len()
    ));

    // ③ compact：既有消息打标记 + 追加摘要，一次变更全生效。
    let summary = BaseMessage::ai("synthetic summary");
    facade
        .apply_compaction(
            &root,
            &CompactionChange {
                flag_updates: vec![(
                    first.id(),
                    peri_acp_types::store::MessageFlags {
                        truncated: false,
                        excluded: true,
                        projection: None,
                    },
                )],
                appended_messages: vec![summary],
            },
        )
        .await
        .map_err(failure)?;
    let compacted = facade.load_session_history(&root).await.map_err(failure)?;
    lines.push(format!("compact_rows={}", compacted.len()));

    // ④ fork：目标快照独立落库（source 不变）。
    //
    // `message_id` 是库级主键（本机与远端同一形状），复制 source 历史必须**重映射 ID**；
    // 复用原 id 会撞主键。产品路径在派发层先做纯 ID 重映射，这里先证明复用被**明确拒绝**
    // （不是静默丢行），再用重映射后的快照成功落库。
    let fork = context.thread(FORK_SUFFIX);
    let reused = facade
        .save_fork(&ForkSnapshot {
            target: session_input(context, &fork, &workspace, None),
            source_id: root.clone(),
            payloads: compacted.clone(),
            flags: std::collections::HashMap::new(),
        })
        .await;
    check(
        matches!(
            reused.as_ref().map_err(|error| error.kind()),
            Err(SessionResourceErrorKind::InvalidInput { .. })
        ),
        "reusing source message ids in a fork must be refused, not silently dropped",
    )?;
    let fork_lease = facade
        .save_fork(&ForkSnapshot {
            target: session_input(context, &fork, &workspace, None),
            source_id: root.clone(),
            payloads: remap_for_fork(&compacted),
            flags: std::collections::HashMap::new(),
        })
        .await
        .map_err(failure)?;

    // ⑤ child：继承区 + 父子关系，沿用 root owner 与 root 的 frozen 原文。
    let child = context.thread(CHILD_SUFFIX);
    let root_frozen = frozen_of(&facade, &root).await?;
    check(
        root_frozen == synthetic_frozen(context),
        "the frozen snapshot must round-trip through the remote store",
    )?;
    facade
        .save_child(
            &peri_acp_types::session_resources::ChildSnapshot {
                target: child_input(context, &child, &workspace, &root, root_frozen),
                parent_id: root.clone(),
                root_id: root.clone(),
                inherited: InheritedContext {
                    payloads: compacted,
                    flags: std::collections::HashMap::new(),
                },
            },
            &lease,
        )
        .await
        .map_err(failure)?;
    lines.push(format!(
        "tree_sessions={} children={}",
        facade
            .list_session_tree(&root)
            .await
            .map_err(failure)?
            .len(),
        facade.list_children(&root).await.map_err(failure)?.len()
    ));

    // ⑥ 标题 A→B→A：第三次领域调用必须落地（同内容不是「历史重放」，是新的领域调用）。
    //    操作身份由每次调用铸造，不由内容派生——这里正是它的端到端回归。
    for title in ["title-a", "title-b", "title-a"] {
        facade
            .update_session_meta(
                &root,
                &SessionMetaPatch {
                    title: Some(Some(title.to_owned())),
                    ..Default::default()
                },
            )
            .await
            .map_err(failure)?;
    }
    let meta = facade.load_session_meta(&root).await.map_err(failure)?;
    lines.push(format!(
        "title_after_aba={}",
        meta.title.as_deref().unwrap_or("-")
    ));
    check(
        meta.title.as_deref() == Some("title-a"),
        "the third update (same title as the first) must still apply",
    )?;

    // ⑦ 关闭：返回后不再接受新写入。进程在这里退出，**不写 clean**：执行代际留在本机，
    //    下一个进程因此必须走「先收敛未决、再解除 ordinary dirty」的正路。
    facade.close().await.map_err(failure)?;
    drop((lease, fork_lease));
    Ok(lines)
}

// ─── 子进程 B：冷恢复 → rewind/delete ─────────────────────────────────────────

#[tokio::test]
async fn cloud_deployment_child_recovers_cold_then_rewinds_and_deletes() {
    let Some(context) = child_context() else {
        return;
    };
    let target = CloudTarget::load();
    let mut out = target.out();
    let lines = recover_flow(&context, &target)
        .await
        .unwrap_or_else(|message| panic!("{message}"));
    report(&mut out, &lines);
    out.push("recover_phase=ok".to_owned());
    out.flush();
}

async fn recover_flow(context: &ChildContext, target: &CloudTarget) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let facade = open_facade(context, AccessMode::ReadWrite, target).await?;
    let root = context.thread(ROOT_SUFFIX);
    let child = context.thread(CHILD_SUFFIX);
    let fork = context.thread(FORK_SUFFIX);

    // ① 未决收敛：上一个进程没有未结清的写入 ⇒ `Recovered`（不是「目标读不到就算没发生」）。
    let recovery = facade
        .recover_session_persistence(&root)
        .await
        .map_err(failure)?;
    check(
        recovery == PersistenceRecovery::Recovered,
        "cold recovery must converge with no unsettled operations",
    )?;

    // ② 上一个进程异常退出留下的 ordinary dirty：先收敛，再按显式风险接受解除。
    let availability = facade
        .inspect_availability(Some(&root))
        .await
        .map_err(failure)?;
    let Some(ExecutionAvailability::Dirty(details)) = availability.execution else {
        return Err(format!(
            "a process that exited without clean must report dirty: {:?}",
            availability.execution
        ));
    };
    facade
        .reset_dirty_execution(&ResetDirtyRequest {
            target: details,
            accept_risk: true,
        })
        .await
        .map_err(failure)?;
    lines.push("dirty=reset".to_owned());

    let workspace = facade
        .resolve_workspace(&context.workspace)
        .await
        .map_err(failure)?;
    let lease = facade
        .acquire_execution(&root, &workspace)
        .await
        .map_err(failure)?;

    // ③ rewind：保留到第一条消息（显式边界），派生计数随之更新。
    let history = facade.load_session_history(&root).await.map_err(failure)?;
    check(
        history.len() >= 2,
        "cold read must return the saved history",
    )?;
    facade
        .rewind_history(&root, RewindBoundary::KeepThrough(history[0].id()))
        .await
        .map_err(failure)?;
    let rewound = facade.load_session_history(&root).await.map_err(failure)?;
    lines.push(format!("rewind_rows={}", rewound.len()));
    check(rewound.len() == 1, "rewind must keep exactly one message")?;

    // ④ 删除整棵树：root 与 child 一起消失，fork 目标是另一棵树，必须仍在。
    facade.delete_session_tree(&root).await.map_err(failure)?;
    check(
        matches!(
            facade.load_session_meta(&root).await.unwrap_err().kind(),
            SessionResourceErrorKind::NotFound
        ),
        "the deleted root must be gone",
    )?;
    check(
        matches!(
            facade
                .load_session_history(&child)
                .await
                .unwrap_err()
                .kind(),
            SessionResourceErrorKind::NotFound
        ),
        "the deleted tree must take its child",
    )?;
    check(
        !facade
            .load_session_history(&fork)
            .await
            .map_err(failure)?
            .is_empty(),
        "an unrelated tree must survive the deletion",
    )?;
    lines.push("delete=applied".to_owned());

    // ⑤ 终态：删除留下的墓碑不阻止 clean 落盘。
    lease
        .mark_clean()
        .await
        .map_err(|error| error.to_string())?;
    facade.close().await.map_err(failure)?;
    Ok(lines)
}

// ─── 子进程 C/D：显式只读 ─────────────────────────────────────────────────────

#[tokio::test]
async fn cloud_deployment_child_read_only_leaves_no_trace() {
    let Some(context) = child_context() else {
        return;
    };
    let Ok(mode) = std::env::var(READ_ONLY_ENV) else {
        return;
    };
    let target = CloudTarget::load();
    let mut out = target.out();
    let lines = read_only_flow(&context, &mode, &target)
        .await
        .unwrap_or_else(|message| panic!("{message}"));
    report(&mut out, &lines);
    out.push(format!("read_only_{mode}=ok"));
    out.flush();
}

async fn read_only_flow(
    context: &ChildContext,
    mode: &str,
    target: &CloudTarget,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let before = listing(&context.home)?;

    // `fresh`：本机没有执行事实库。只读意图不许创建它，因此这次打开在建立任何远端连接
    // 之前就如实失败——与本机库「只读打开一个不存在的库」是同一个判定（`NotFound`），
    // 两种存储模式下这句话必须是同一句。
    if mode == "fresh" {
        let error = match open_facade_error(context, AccessMode::ReadOnly).await {
            Ok(_) => {
                return Err("a read-only open without local execution facts must fail".to_owned())
            }
            Err(error) => error,
        };
        let failure = crate::classify_open_failure(&error);
        check(
            failure == crate::StoreOpenFailure::NotFound,
            &format!("a missing local execution face must open as NotFound, got {failure:?}"),
        )?;
        let after = listing(&context.home)?;
        check(
            before == after,
            &format!(
                "a refused read-only open must not create local files: before={before:?} after={after:?}"
            ),
        )?;
        lines.push(format!("read_only_mode={mode} refusal={failure:?}"));
        return Ok(lines);
    }

    let facade = open_facade(context, AccessMode::ReadOnly, target).await?;
    let availability = facade.inspect_availability(None).await.map_err(failure)?;
    check(
        availability.access == AccessMode::ReadOnly
            && availability.capabilities == DataCapabilities::HistoryReadOnly,
        "read-only intent must select the read-only capability face",
    )?;

    let fork = context.thread(FORK_SUFFIX);
    let history = facade.load_session_history(&fork).await.map_err(failure)?;
    check(
        !history.is_empty(),
        "read-only open must still read the remote session data",
    )?;

    let fork_availability = facade
        .inspect_availability(Some(&fork))
        .await
        .map_err(failure)?;
    // 本机执行事实在，但这次是只读打开：执行权一律不可得，如实回答 `ReadOnlyStore`
    // （历史照常可读）。有本机库不改变只读这件事。
    check(
        fork_availability.execution == Some(ExecutionAvailability::ReadOnlyStore),
        "a read-only open must report the read-only execution face",
    )?;

    // 写入在副作用之前被拒绝：类型化拒绝，不假装成功。
    let error = facade
        .update_session_meta(
            &fork,
            &SessionMetaPatch {
                title: Some(Some("must not apply".to_owned())),
                ..Default::default()
            },
        )
        .await
        .expect_err("read-only open must refuse writes");
    check(
        matches!(error.kind(), SessionResourceErrorKind::ReadOnlyStore),
        &format!(
            "read-only refusal must be typed ({mode}): {}",
            remote_error_class(&error)
        ),
    )?;
    lines.push(format!(
        "read_only_mode={mode} rows={} refusal={}",
        history.len(),
        remote_error_class(&error)
    ));

    facade.close().await.map_err(failure)?;
    // 只读打开与只读拒绝都不得在本机留下任何文件（本机库、锁）。
    let after = listing(&context.home)?;
    check(
        before == after,
        &format!("a read-only open must not create local files: before={before:?} after={after:?}"),
    )?;
    Ok(lines)
}

// ─── 共用小工具 ────────────────────────────────────────────────────────────────

/// 本机目录内容（含子目录文件名），用于「只读不建文件」的前后比对。
fn listing(root: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            names.push(path.display().to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// 真实部署入口：与 CLI/TUI/print/stdio/meta 走的是同一个装配点。
async fn open_facade(
    context: &ChildContext,
    access: AccessMode,
    _target: &CloudTarget,
) -> Result<std::sync::Arc<SessionResourcesImpl>, String> {
    open_facade_error(context, access)
        .await
        .map_err(|error| format!("deployment open failed: {error:#}"))
}

/// 同一个入口，保留类型化错误：拒绝的判定（`StoreOpenFailure`）只能按类型做，
/// 不解析错误文本。
async fn open_facade_error(
    context: &ChildContext,
    access: AccessMode,
) -> Result<std::sync::Arc<SessionResourcesImpl>, anyhow::Error> {
    let deployment = context.deployment(access).map_err(anyhow::Error::msg)?;
    let resources = crate::Resources::open_deployment(&deployment).await?;
    Ok(resources.into_concrete_for_test())
}

fn binding_of(workspace: &peri_acp_types::workspace::ResolvedWorkspace) -> SessionBinding {
    SessionBinding {
        schema_version: SESSION_BINDING_VERSION,
        revision: 1,
        project_id: workspace.project_id,
        workspace_id: workspace.workspace_id,
        cwd_relative_to_workspace: workspace.relative_cwd.clone(),
    }
}

/// 合成会话输入：内容全部由本轮 run 派生，cwd 是本轮合成的 workspace。
fn session_input(
    context: &ChildContext,
    thread: &ThreadId,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    parent: Option<&ThreadId>,
) -> NewSession {
    NewSession {
        thread_id: thread.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: Some(format!("synthetic {thread}")),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: parent.cloned(),
            hidden: parent.is_some(),
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: binding_of(workspace),
        frozen: synthetic_frozen(context),
    }
}

/// fork 目标的历史：**ID 重映射**后复制（与 `peri-acp::dispatch::session_fork` 同一语义，
/// 那里是产品路径的唯一实现；adapter 不重复 fork 算法）。
fn remap_for_fork(payloads: &[PersistedPayload]) -> Vec<PersistedPayload> {
    payloads
        .iter()
        .map(|payload| match payload {
            PersistedPayload::Message(BaseMessage::Human { content, .. }) => {
                PersistedPayload::Message(BaseMessage::Human {
                    id: MessageId::new(),
                    content: content.clone(),
                })
            }
            PersistedPayload::Message(BaseMessage::Ai {
                content,
                tool_calls,
                ..
            }) => PersistedPayload::Message(BaseMessage::Ai {
                id: MessageId::new(),
                content: content.clone(),
                tool_calls: tool_calls.clone(),
            }),
            // 本轮合成历史只有 user/assistant 两类；原样复制其他形态会撞主键，
            // 因此这里明确不猜（真需要时按同一规则补全重映射）。
            other => panic!("synthetic fork history has no remapping rule: {other:?}"),
        })
        .collect()
}

fn synthetic_frozen(context: &ChildContext) -> FrozenSnapshotBytes {
    FrozenSnapshotBytes::new(format!("{{\"frozen\":\"{}\"}}", context.run))
}

/// child 目标：父子身份 + **逐字节沿用** root 已保存的 frozen 原文。
fn child_input(
    context: &ChildContext,
    thread: &ThreadId,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
    root: &ThreadId,
    frozen: FrozenSnapshotBytes,
) -> NewSession {
    NewSession {
        frozen,
        ..session_input(context, thread, workspace, Some(root))
    }
}

/// root 已保存的 frozen 原文（child 必须逐字节沿用，不重新构造）。
async fn frozen_of(
    facade: &SessionResourcesImpl,
    id: &ThreadId,
) -> Result<FrozenSnapshotBytes, String> {
    match facade
        .load_session_snapshot(id)
        .await
        .map_err(failure)?
        .frozen
    {
        FrozenState::Present(bytes) => Ok(bytes),
        state => Err(format!("root frozen snapshot is missing: {state:?}")),
    }
}
