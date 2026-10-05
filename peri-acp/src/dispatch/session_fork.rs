//! Fork 普通会话：一致 source 快照 → 领域纯 ID 映射 → 一次门面保存。
//!
//! 存储访问经会话资源门面（ARC-BOUNDARY-001 方向）：ACP 不逐条写 flags、不拼
//! create/append/flags 分步序列，也没有「复制失败再删除新 thread」的存储补偿——
//! 目标快照由 [`SessionResources::save_fork`] 一次保存。
//! 未发布创建的撤销由调用方经 `abandon_initialization` 承担。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{
    BindingState, ForkSnapshot, FrozenSnapshotBytes, FrozenState, NewSession, NewSessionMeta,
    SessionResources, SessionSnapshot,
};
use peri_acp_types::store::history::remap_fork_history;
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};

/// fork source 的一致快照：payload/flags/binding/frozen 同一时刻读出。
///
/// source 原样保留——此处只读，不改动 source 的 flags 或 frozen。
pub(crate) struct ForkSource {
    pub(crate) thread_id: ThreadId,
    /// source 已登记的绑定：普通 fork 沿用同一执行绑定。
    pub(crate) binding: SessionBinding,
    /// source 已持久化的精确冻结字节：fork 不按当前目录/日期重冻。
    pub(crate) frozen: FrozenSnapshotBytes,
    payloads: Vec<PersistedPayload>,
    flags: HashMap<MessageId, MessageFlags>,
}

/// 读取 fork source：一次一致快照，并在领域侧校验可 fork 性。
///
/// source 必须是已绑定、frozen 可读且工具调用已闭合的会话；缺绑定、frozen 缺失或
/// 本构建读不懂时明确失败，不用当前环境补一份。
pub(crate) async fn load_fork_source(
    resources: &Arc<dyn SessionResources>,
    source_thread_id: &str,
) -> Result<ForkSource> {
    let thread_id = source_thread_id.to_owned();
    let snapshot = resources.load_session_snapshot(&thread_id).await?;
    let binding = bound_binding(&snapshot)?;
    let frozen = source_frozen(&snapshot)?;
    let payloads = ensure_complete_tool_calls(snapshot.payloads)?;
    Ok(ForkSource {
        thread_id,
        binding,
        frozen,
        payloads,
        flags: snapshot.flags,
    })
}

/// 一次保存 fork 目标：纯 ID 映射在前，门面保存 meta/binding/frozen/payload/flags。
///
/// 目标复用 source 的 binding 与冻结字节；`created_at` 与目标 identity 由构建层
/// 生成一次（重试不重建）。
pub(crate) async fn fork_bound_session(
    resources: &Arc<dyn SessionResources>,
    source: &ForkSource,
    workspace: &ResolvedWorkspace,
    created_at: String,
) -> Result<(String, Vec<PersistedPayload>)> {
    let cwd = workspace
        .cwd
        .to_str()
        .context("Execution directory is not UTF-8")?
        .to_owned();
    // SQLite 的 message_id 是库级主键：复用 source ID 会让写入静默丢行，因此复制前
    // 先做纯 ID 重映射（adapter 不重复执行 fork 算法）。
    let forked = remap_fork_history(&source.payloads, &source.flags, MessageId::new);
    let target_id = uuid::Uuid::now_v7().to_string();
    resources
        .save_fork(&ForkSnapshot {
            target: NewSession {
                thread_id: target_id.clone(),
                created_at,
                meta: NewSessionMeta {
                    title: None,
                    cwd,
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: CancelPolicy::default(),
                    snapshot_at_message_id: None,
                },
                binding: source.binding.clone(),
                frozen: source.frozen.clone(),
            },
            source_id: source.thread_id.clone(),
            payloads: forked.payloads.clone(),
            flags: forked.flags,
        })
        .await?;
    tracing::info!(
        source = %source.thread_id,
        new = %target_id,
        msg_count = forked.payloads.len(),
        "Session forked"
    );
    Ok((target_id, forked.payloads))
}

/// 已绑定 source 才可 fork：legacy/外来/缺绑定都不是「可复制的执行身份」。
fn bound_binding(snapshot: &SessionSnapshot) -> Result<SessionBinding> {
    match &snapshot.binding {
        BindingState::Bound(binding) => Ok(binding.clone()),
        BindingState::LegacyConfirmed => bail!("Cannot fork legacy history without a binding"),
        BindingState::ExternalOrUnregistered => {
            bail!("Cannot fork a session bound to another workspace")
        }
        BindingState::Missing => bail!("Cannot fork a session without a local binding"),
    }
}

/// fork 复制的是 source 的精确冻结字节，来源必须已持久化且可读。
fn source_frozen(snapshot: &SessionSnapshot) -> Result<FrozenSnapshotBytes> {
    match &snapshot.frozen {
        FrozenState::Present(bytes) => Ok(bytes.clone()),
        FrozenState::LegacyAbsent => bail!("Source frozen snapshot is missing"),
        FrozenState::Unsupported => {
            bail!("Source frozen snapshot is not readable by this build")
        }
    }
}

/// 工具调用闭合校验：未闭合的调用不能复制进一条独立可执行的历史。
fn ensure_complete_tool_calls(payloads: Vec<PersistedPayload>) -> Result<Vec<PersistedPayload>> {
    let mut pending = HashSet::new();
    for message in payloads.iter().filter_map(PersistedPayload::as_message) {
        for call in message.tool_calls() {
            pending.insert(call.id.clone());
        }
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            pending.remove(tool_call_id);
        }
    }
    if !pending.is_empty() {
        bail!("Cannot fork history with incomplete tool calls");
    }
    Ok(payloads)
}
