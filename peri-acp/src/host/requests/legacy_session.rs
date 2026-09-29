//! On-demand compatibility for unbound roots. Listing and history replay do not enter here.

use peri_acp_types::{
    session_resources::{BindingState, FrozenSnapshotBytes, FrozenState},
    thread::ThreadMeta,
    workspace::{ResolvedWorkspace, WorkspaceError},
};
use std::path::Path;

use super::{decode_frozen_snapshot, AcpError, AcpServerConfig};
use crate::host::prepared::PreparedSessionInputs;
use crate::host::workspace::{resource_error, workspace_error};

pub(super) async fn resolve_saved_workspace(
    cfg: &AcpServerConfig,
    meta: &ThreadMeta,
) -> Result<ResolvedWorkspace, AcpError> {
    if meta.parent_thread_id.is_some() {
        return Err(workspace_error(WorkspaceError::ExecutionLeaseRequired));
    }
    if !Path::new(&meta.cwd).is_absolute() {
        return Err(workspace_error(WorkspaceError::Unavailable));
    }
    cfg.session_resources
        .resolve_workspace(Path::new(&meta.cwd))
        .await
        .map_err(resource_error)
}

/// 恢复前的 legacy 准备：已绑定会话返回 `None`。
///
/// 绑定分类与 frozen 状态来自门面的一次一致读取：只有本机确认的 legacy
/// （[`BindingState::LegacyConfirmed`]）才进入接纳，外来登记、缺登记与损坏不会被
/// 解释成「没有 legacy」。无 frozen 的 legacy 会话按保存的绝对 cwd 只读定格一份完整
/// 准备输入——插件发现走严格只读入口，不生成合成清单、不写插件缓存。
pub(super) async fn prepare_for_restore(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected_cwd: Option<&str>,
) -> Result<Option<PreparedSessionInputs>, AcpError> {
    let resources = cfg.session_resources.clone();
    let id = session_id.to_owned();
    let mut snapshot = resources
        .load_session_snapshot(&id)
        .await
        .map_err(resource_error)?;
    let meta = snapshot.meta.clone();
    // 本机 legacy 的证据是「保存的绝对 cwd 落在已登记 workspace 之下」。新库/新节点
    // 上该目录尚未登记，Missing 因此先按保存路径解析登记一次再复判——不把「尚未登记」
    // 直接当成本机 legacy，已绑定会话也不为这一轮多跑发现。
    let workspace = match snapshot.binding {
        BindingState::Bound(_) => return Ok(None),
        BindingState::LegacyConfirmed => resolve_saved_workspace(cfg, &meta).await?,
        BindingState::Missing => {
            let workspace = resolve_saved_workspace(cfg, &meta).await?;
            snapshot = resources
                .load_session_snapshot(&id)
                .await
                .map_err(resource_error)?;
            if !matches!(snapshot.binding, BindingState::LegacyConfirmed) {
                return Err(workspace_error(WorkspaceError::Unavailable));
            }
            workspace
        }
        // 外来会话或本机登记缺失：历史可读，但不能按 legacy 自动接纳。
        BindingState::ExternalOrUnregistered => {
            return Err(workspace_error(WorkspaceError::Unavailable))
        }
    };
    if let Some(expected) = expected_cwd {
        // 与绑定比对的是「同一目录」，不需要为此再解析登记（那会多跑一轮完整发现）。
        crate::host::workspace::expect_directory(expected, &workspace).await?;
    }
    // frozen 呈现两条事实源：持久字节（Present）当场接纳；LegacyAbsent 在接纳事务前
    // 构建一次候选（J2 §3.1：legacy 不走两阶段——存储要求绑定先于执行所有权，接纳
    // 之前无法取得执行环境，因此这里没有 MCP 资源面可用；覆盖不可得按 X8 保持内置）。
    let (frozen, prepared) = match snapshot.frozen {
        FrozenState::Present(bytes) => {
            decode_frozen_snapshot(bytes.as_str()).map_err(workspace_error)?;
            (bytes, None)
        }
        FrozenState::LegacyAbsent => {
            // Legacy sessions never captured this state. Freeze from their saved cwd once,
            // as the pre-3.15 compatibility path did; never use the caller's terminal cwd.
            let workspace_cwd = workspace.cwd.to_string_lossy().into_owned();
            let prepared = PreparedSessionInputs::prepare_legacy(cfg, &meta.cwd, &workspace_cwd)?;
            let bytes = prepared.frozen_encoded.clone().ok_or_else(|| {
                AcpError::new(-32603, "Legacy frozen candidate bytes are missing")
            })?;
            (FrozenSnapshotBytes::new(bytes), Some(prepared))
        }
        // 有快照但本构建读不懂：不是「缺失」，不能按 legacy 规则重冻覆盖既有字节。
        FrozenState::Unsupported => {
            return Err(AcpError::new(
                -32603,
                "Session frozen snapshot is not readable by this build",
            ))
        }
    };
    // 接纳在门面的一次行为里完成（binding 与缺失的 frozen 一起成立）；登记的保存目录
    // 来自准备输入（legacy 只按保存的绝对 cwd 记录事实）。
    let recorded_cwd = prepared
        .as_ref()
        .and_then(|inputs| inputs.legacy.as_ref())
        .map(|legacy| legacy.saved_cwd.to_string_lossy().into_owned())
        .unwrap_or_else(|| meta.cwd.clone());
    resources
        .adopt_legacy_session(&id, &recorded_cwd, &workspace, &frozen)
        .await
        .map_err(resource_error)?;
    Ok(prepared)
}
