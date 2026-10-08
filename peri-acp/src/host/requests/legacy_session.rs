//! On-demand compatibility for unbound roots. Listing and history replay do not enter here.
//!
//! M5：legacy 首次接纳的顺序是「执行资格 → 仅资源 bootstrap → 读取指引/技能/覆盖
//! 文档 → 定稿 frozen → 原子 write-once 接纳（frozen+binding）→ 发布」。bootstrap
//! 只装配资源环境（MCP 池 + builtin workspace 实例），**不运行 Agent / 项目 hook**；
//! 竞争输家不改写既有字节，按 winner 的持久字节重新定格输入。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::{
    session_resources::{BindingState, FrozenSnapshotBytes, FrozenState},
    thread::ThreadMeta,
    workspace::ResolvedWorkspace,
};

use super::{decode_frozen_snapshot, AcpError, AcpServerConfig};
use crate::host::prepared::PreparedSessionInputs;
use crate::host::requests::session_lifecycle::enabled_meta_sections;
use crate::host::workspace::{resource_error, workspace_error, SessionEnvironment};

/// legacy 首次接纳的定稿结果：已按接纳后（或 winner）的字节构建的准备输入。
pub(super) struct LegacyAdoption {
    pub(super) prepared: PreparedSessionInputs,
    /// 阶段一已装配并 activate 的环境（与 `prepared` 的 frozen 同源）。竞争者胜出
    /// 且字节一致时复用，避免第二次装配；否则为 None（调用方按 winner 输入装配）。
    pub(super) environment: Option<Arc<SessionEnvironment>>,
}

pub(super) async fn resolve_saved_workspace(
    cfg: &AcpServerConfig,
    meta: &ThreadMeta,
) -> Result<ResolvedWorkspace, AcpError> {
    cfg.session_resources
        .validate_bound_workspace(
            &meta.id,
            peri_acp_types::session_resources::BindingRecheck::Recorded,
        )
        .await
        .map_err(resource_error)
}

/// 读取接纳后的持久 frozen 字节（write-once 后唯一事实源）。
async fn persisted_frozen_bytes(
    resources: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    id: &str,
) -> Result<String, AcpError> {
    let snapshot = resources
        .load_session_snapshot(&id.to_owned())
        .await
        .map_err(resource_error)?;
    match snapshot.frozen {
        FrozenState::Present(bytes) => Ok(bytes.into_string()),
        FrozenState::LegacyAbsent => Err(AcpError::new(
            -32603,
            "Legacy adoption did not commit a frozen snapshot",
        )),
    }
}

/// 读失败时排空已装配的环境，绝不留下未接纳的半成品（也不重冻覆盖既有字节）。
/// 保留原始错误原因；清理失败显式并入错误，不吞也不替换为无上下文的新错误。
async fn drain_on_failure(
    environment: Option<&Arc<SessionEnvironment>>,
    error: AcpError,
) -> AcpError {
    if let Some(environment) = environment {
        if !environment.shutdown().await {
            return AcpError::new(
                -32603,
                format!(
                    "{}; additionally the legacy bootstrap environment did not confirm shutdown; nothing was adopted",
                    error.message
                ),
            );
        }
    }
    error
}

/// 恢复前的 legacy 准备：已绑定会话返回 `None`。
///
/// 绑定分类与 frozen 状态来自门面的一次一致读取。调用方先确认机器环境可执行（
/// 只读存储与远端执行环境在更早的资格检查被拒，不会走到本函数）。无 frozen 的
/// legacy 会话按保存的 cwd 接纳：先装配资源环境并读取内容，再以**定稿 frozen +
/// binding** 作为一次 write-once 接纳；竞争输家读取 winner 字节重新定格。
pub(super) async fn prepare_for_restore(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected_cwd: Option<&str>,
    effective_servers: HashMap<String, peri_acp_types::plugin::McpServerConfig>,
) -> Result<Option<LegacyAdoption>, AcpError> {
    let resources = cfg.session_resources.clone();
    let id = session_id.to_owned();
    let snapshot = resources
        .load_session_snapshot(&id)
        .await
        .map_err(resource_error)?;
    let meta = snapshot.meta.clone();
    if matches!(snapshot.binding, BindingState::Bound(_)) {
        return Ok(None);
    }
    let _ = expected_cwd;
    // 执行资格（本机可用执行目录）先于任何读取：异机/远端执行环境在此失败，
    // 不会去读本机同名路径。
    let workspace = resolve_saved_workspace(cfg, &meta).await?;
    let workspace_cwd = workspace.cwd.to_string_lossy().into_owned();
    match snapshot.frozen {
        // frozen 已存在但绑定缺失：接纳只补绑定并保持既有字节（write-once），随后
        // 按 winner 字节重新定格输入（不重建、不覆盖）。未知版本的唯一错误权威是
        // 解码器（`FrozenSnapshotError::UnsupportedVersion`），存储层不表达它。
        FrozenState::Present(bytes) => {
            decode_frozen_snapshot(bytes.as_str()).map_err(workspace_error)?;
            resources
                .adopt_legacy_session(&id, &meta.cwd, &workspace, &bytes)
                .await
                .map_err(resource_error)?;
            let persisted = persisted_frozen_bytes(&resources, &id).await?;
            let mut prepared =
                PreparedSessionInputs::prepare_restore(cfg, &workspace_cwd, &persisted)?;
            prepared.session_mcp_servers = effective_servers;
            Ok(Some(LegacyAdoption {
                prepared,
                environment: None,
            }))
        }
        FrozenState::LegacyAbsent => {
            let mut prepared =
                PreparedSessionInputs::prepare_legacy_deferred(cfg, &meta.cwd, &workspace_cwd)?;
            // 有效 servers（持久 owner 声明 + 本次请求一致性校验后的结果）必须在
            // bootstrap 之前定格：环境的会话 MCP 池只在装配时消费一次（OnceLock），
            // 事后写入 prepared 不会再生效。
            prepared.session_mcp_servers = effective_servers.clone();
            // 仅资源 bootstrap：装配会话环境（MCP 池 + builtin 实例）并 activate，
            // 不运行 Agent / 项目 hook，也不把临时候选冻结成最终空快照。
            let environment =
                match SessionEnvironment::assemble_prepared_without_frozen(cfg, &prepared, &id)
                    .await
                {
                    Ok(environment) => environment,
                    Err(error) => return Err(error),
                };
            if let Some(environment) = &environment {
                environment.activate();
            }
            if environment.is_none() {
                // 显式缺口：无资源面（无 session-scoped workspace）时项目指引与技能
                // 摘要不可得——保持空值并留信号，不回落磁盘（X4/J5）。
                tracing::warn!(
                    cwd = %workspace_cwd,
                    "legacy 首次接纳无资源环境：项目指令与技能摘要不可得（不回落磁盘）"
                );
            }
            // bootstrap 之后的每一步（读取 → 定稿 → 接纳 → winner 重读）都必须在
            // 失败时排空已激活环境：候选环境由本作用域唯一持有，成功路径才把所有权
            // 交给调用方。
            let mut environment = environment;
            let outcome: Result<LegacyAdoption, AcpError> = async {
                // 内容准入（与 session/new 的 P4 同源）：覆盖文档 / 技能目录 / 项目指令。
                let enabled_sections = match environment.as_ref() {
                    Some(env) => enabled_meta_sections(&env.cfg),
                    None => enabled_meta_sections(cfg),
                };
                let docs = match environment.as_ref() {
                    Some(env) if !enabled_sections.is_empty() => {
                        env.read_meta_docs(&enabled_sections).await?
                    }
                    _ => HashMap::new(),
                };
                let skill_catalog = match environment.as_ref() {
                    Some(env) => env.read_workspace_skill_catalog().await?,
                    None => Vec::new(),
                };
                let instructions = match environment.as_ref() {
                    Some(env) => env.read_workspace_instructions().await?,
                    None => Default::default(),
                };
                let build_cfg: &AcpServerConfig = match environment.as_ref() {
                    Some(env) => &env.cfg,
                    None => cfg,
                };
                prepared.build_frozen_after_activation(
                    build_cfg,
                    docs,
                    &skill_catalog,
                    &instructions,
                )?;
                let candidate = prepared.frozen_encoded.clone().ok_or_else(|| {
                    AcpError::new(-32603, "Legacy frozen candidate bytes are missing")
                })?;
                let candidate_bytes = FrozenSnapshotBytes::new(candidate);
                // 原子 write-once 接纳：frozen 与 binding 一起成立；已有绑定/字节的
                // 竞争输家不改写 winner。
                resources
                    .adopt_legacy_session(&id, &meta.cwd, &workspace, &candidate_bytes)
                    .await
                    .map_err(resource_error)?;
                let persisted = persisted_frozen_bytes(&resources, &id).await?;
                if persisted == candidate_bytes.as_str() {
                    let environment = environment.take();
                    return Ok(LegacyAdoption {
                        prepared,
                        environment,
                    });
                }
                // 竞争输家：丢弃本次候选环境（排空确认），按 winner 的持久字节重新
                // 定格（装配交回调用方，用 winner 输入做第二次装配）。
                if let Some(environment) = environment.take() {
                    if !environment.shutdown().await {
                        return Err(AcpError::new(
                            -32603,
                            "legacy adoption lost the race and bootstrap resources did not confirm shutdown",
                        ));
                    }
                }
                let mut prepared =
                    PreparedSessionInputs::prepare_restore(cfg, &workspace_cwd, &persisted)?;
                prepared.session_mcp_servers = effective_servers;
                Ok(LegacyAdoption {
                    prepared,
                    environment: None,
                })
            }
            .await;
            match outcome {
                Ok(adoption) => Ok(Some(adoption)),
                Err(error) => Err(drain_on_failure(environment.as_ref(), error).await),
            }
        }
    }
}
