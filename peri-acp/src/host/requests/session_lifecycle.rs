//! Session 生命周期命令 handler：initialize / new / load / list /
//! cancel-bg-task / close / delete / resume / fork / rename（自 requests.rs
//! 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CloseSessionResponse, DeleteSessionResponse, ForkSessionResponse, ListSessionsResponse,
    NewSessionResponse, SessionId,
};
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, FrozenState, NewSessionDraft, NewSessionMeta, SessionInitialization,
    SessionMetaPatch,
};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{info, warn};

use super::super::notify::send_available_commands_update;
use super::super::{build_mode_state, AcpServerConfig, SessionState};
use crate::dispatch::config_update::make_config_options;
use crate::{dispatch, transport::types::AcpError};

#[path = "session_restore.rs"]
pub(crate) mod restore;
pub(crate) use restore::{handle_context, handle_load, handle_metadata, handle_resume};
use restore::{identity_response, prepare_existing, response_identity};

/// fork source 读取/保存失败 → ACP 错误。
///
/// 门面失败保留领域分类（含访问权限与未决持久化状态）；领域与 IO 失败按
/// workspace 语义上报，不把门面错误降级成「存储不可用」。
fn fork_source_error(error: anyhow::Error) -> AcpError {
    match error.downcast::<peri_acp_types::session_resources::SessionResourceError>() {
        Ok(error) => super::super::workspace::resource_error(error),
        Err(error) => super::super::workspace::workspace_error(error),
    }
}

/// 创建 session 级 WorkflowMiddleware（session/new / load / resume 共用，GAP-05）。
///
/// 构造收拢在 host 装配面（`host/workflow_agent.rs` 薄壳：executor 注入面 +
/// 端口装配），命令面只持 `Arc<dyn WorkflowMiddlewarePort>`（3.0 批 2
/// 波 2 装配边界收口；p1-wa：执行体在 peri-agent，装配经
/// `workflow_middleware_factory` 端口）。
pub(super) fn create_session_workflow_middleware(
    cfg: &AcpServerConfig,
    cwd: &str,
    session_id: &str,
    frozen_data: &crate::session::executor::FrozenSessionData,
) -> Option<Arc<dyn WorkflowMiddlewarePort>> {
    let middleware = crate::host::workflow_agent::create_session_workflow_middleware(
        Arc::clone(&cfg.provider),
        &cfg.peri_config,
        cwd,
        session_id,
        frozen_data,
        Arc::clone(&cfg.workflow_middleware_factory),
        // session 级路径与迁移前一致，不启用事件发布（workflow 事件仅由
        // 内部 handler 消费：usage/progress）；统一发射接线留待单独裁定。
        None,
        Arc::clone(&cfg.agent_catalog),
        // W4b（F4）：workflow agent 与主链共用会话级 MCP skill registry
        // （会话已登记时取到；未登记/print 模式为 None → 技能工具为空面）。
        cfg.session_manager.mcp_skill_registry_for(session_id),
        cfg.session_resources.clone(),
        cfg.execution_admission_port.clone(),
    );
    if let (Some(middleware), Some(session)) =
        (&middleware, cfg.session_manager.get_session(session_id))
    {
        middleware.set_bg_registry(session.task_manager.clone());
    }
    middleware
}

pub(crate) fn handle_initialize(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    info!(protocol_version = %version, "ACP initialize");

    // 解析 clientCapabilities._meta 中的 peri 自定义 flag
    let peri_caps = params
        .get("clientCapabilities")
        .and_then(|c| c.get("_meta"))
        .and_then(|m| m.as_object())
        .map(PeriCaps::from_client_meta)
        .unwrap_or_default();

    // 暂存 caps，session/new 时 consume
    cfg.session_manager.set_pending_caps(peri_caps.clone());

    let resp = dispatch::build_initialize_response(&peri_caps);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(super) fn enabled_meta_sections(cfg: &AcpServerConfig) -> std::collections::HashSet<String> {
    cfg.peri_config
        .read()
        .config
        .meta_harness
        .as_ref()
        .map(|entries| {
            entries
                .iter()
                .filter(|(section, enabled)| {
                    **enabled
                        && peri_acp_types::meta_harness::SECTION_IDS.contains(&section.as_str())
                })
                .map(|(section, _)| section.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[path = "session_mcp_setup.rs"]
mod session_mcp_setup;
use session_mcp_setup::session_mcp_servers;

pub(crate) async fn handle_new(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let agent_instructions = match params
        .get("_meta")
        .and_then(|meta| meta.get("peri.instructions"))
    {
        None => None,
        Some(Value::String(value)) if value.len() <= 64 * 1024 => Some(value.clone()),
        Some(Value::String(_)) => {
            return Err(AcpError::new(-32602, "peri.instructions exceeds 64 KiB"));
        }
        Some(_) => return Err(AcpError::new(-32602, "peri.instructions must be a string")),
    };
    let requested = params.get("cwd").and_then(Value::as_str).unwrap_or(".");
    let workspace = cfg
        .session_resources
        .resolve_workspace(std::path::Path::new(requested))
        .await
        .map_err(super::super::workspace::resource_error)?;
    let cwd = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    // 只读准备：定格配置/插件/frozen，不创建 thread、
    // 不启动 MCP/hooks，也不写任何会话数据或本机登记。new 路径只在这里准备
    // 一次，发布段消费同一个准备对象。
    let mut prepared =
        super::super::prepared::PreparedSessionInputs::prepare_new_deferred(cfg, &cwd)?;
    prepared.agent_instructions = agent_instructions;
    prepared.session_mcp_servers = session_mcp_servers(params)?;
    new_session_from_prepared(cfg, &workspace, prepared, sessions).await
}

/// `session/new` 的发布段：消费**已定格**的准备输入，一次写出 meta/binding/frozen
/// 随后复核准入、装配环境、发布 live 状态。
///
/// 本函数不读配置、不加载插件、不重建 frozen：保存字节与 live 状态都取自调用方
/// 给定的 `prepared`。测试以自己定格的准备对象直接驱动本函数，因此「保存字节 ==
/// 给定的 frozen 字节」是可断言的；准备之后外部输入若被改写，任何在这里重读或
/// 重建的实现都会产出不同字节而使断言失败。
pub(crate) async fn new_session_from_prepared(
    cfg: &AcpServerConfig,
    workspace: &ResolvedWorkspace,
    mut prepared: super::super::prepared::PreparedSessionInputs,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let resources = cfg.session_resources.clone();
    let cwd = prepared.cwd.clone();
    // ── P1：草稿（frozen 暂空）──
    // 身份一次生成；「未发布创建」的 frozen 由 P5 一次性提交，因此内容准入（本次的
    // 准备产物）与发布之间不存在「保存了半份」的中间态。数据已保存但准入失败
    // （saved_but_not_admitted）原样上报，不谎称「确定未创建」。
    let session_id = uuid::Uuid::now_v7().to_string();
    let initialization = resources
        .begin_initialization(&NewSessionDraft {
            thread_id: session_id.clone(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: cwd.clone(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(workspace),
        })
        .await
        .map_err(super::super::workspace::resource_error)?;
    let thread_id = session_id.clone();
    // 创建后的同一次准入复核：与草稿事务写入的绑定比对已记录证据。
    if let Err(error) = resources.validate_session(&session_id, workspace).await {
        initialization
            .clone()
            .abandon()
            .await
            .map_err(super::super::workspace::resource_error)?;
        return Err(super::super::workspace::resource_error(error));
    }
    let identity = match response_identity(cfg, &session_id).await {
        Ok(identity) => identity,
        Err(error) => {
            initialization
                .clone()
                .abandon()
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    // ── P2：装配（MCP pool 挂起；frozen 消费准备产物，不重建）──
    // 装配失败时环境尚未建立（没有对外资源需要排空），撤销未发布的创建即可。
    let environment =
        match super::super::workspace::SessionEnvironment::assemble_prepared_without_frozen(
            cfg,
            &prepared,
            &session_id,
        )
        .await
        {
            Ok(environment) => environment,
            Err(error) => {
                initialization
                    .clone()
                    .abandon()
                    .await
                    .map_err(super::super::workspace::resource_error)?;
                return Err(error);
            }
        };
    // ── P3：activate（资源准入开始）──
    // B3 口径写死为「P0–P5 发布前」：发布点（P6 的 `sessions.insert`）之前可以发生
    // 准备、装配、activate 与资源读取，**不含**工具执行、模型请求与 hook；此处提前
    // activate 后会话仍不在 `sessions` 表里，尚不可提交执行。
    if let Some(environment) = &environment {
        environment.activate();
    }
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);

    // ── P4：activate 后读取 builtin workspace resources（覆盖文档 + 技能快照），
    //    再构建 frozen ──
    // 读取/构建失败按发布前失败处理（X8 的覆盖不可得走降级；X5 的 system 技能面
    // 读取失败走 J2 补偿 fail-closed）：先排空本次已起的环境，再撤销未发布的创建
    // ——不进入 commit，也不留任何半提交。
    // 无启用 section 时不去等 workspace 连接（覆盖不可得即内置，X8；不扩大请求时延）。
    let enabled_sections = enabled_meta_sections(cfg);
    let docs = match environment.as_ref() {
        Some(env) if !enabled_sections.is_empty() => {
            match env.read_meta_docs(&enabled_sections).await {
                Ok(docs) => docs,
                Err(error) => {
                    drain_and_abandon(environment.as_ref(), &initialization).await?;
                    return Err(error);
                }
            }
        }
        _ => HashMap::new(),
    };
    // W4b（F3/J1）：system 来源（builtin `workspace` 实例）的技能元数据快照——
    // 冻结 system prompt 的技能摘要由它渲染（非 system 来源不进冻结面）。
    // X5 失败语义：技能面不适用（未装配/未连接/未声明/被关闭）⇒ 空快照不是失败；
    // 已声明且被选中却读取失败 ⇒ fail-closed（下面按 J2 补偿后返回错误）。
    let skill_catalog = match environment.as_ref() {
        Some(env) => match env.read_workspace_skill_catalog().await {
            Ok(catalog) => catalog,
            Err(error) => {
                drain_and_abandon(environment.as_ref(), &initialization).await?;
                return Err(error);
            }
        },
        None => Vec::new(),
    };
    // W5（E15）：项目指令（AGENTS.md / CLAUDE.md / CLAUDE.local.md）同样在内容
    // 准入期从 system 来源读取——宿主本地读盘与 `@import` 解析已归 provider。
    let instructions = match environment.as_ref() {
        Some(env) => match env.read_workspace_instructions().await {
            Ok(instructions) => instructions,
            Err(error) => {
                drain_and_abandon(environment.as_ref(), &initialization).await?;
                return Err(error);
            }
        },
        None => Default::default(),
    };
    if let Err(error) =
        prepared.build_frozen_after_activation(cfg, docs, &skill_catalog, &instructions)
    {
        drain_and_abandon(environment.as_ref(), &initialization).await?;
        return Err(error);
    }
    let frozen_data = prepared
        .frozen
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "Frozen snapshot was not built"))?;
    let frozen_encoded = prepared
        .frozen_encoded
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "Frozen snapshot bytes were not built"))?;
    // ── P5：commit_frozen（一次性 CAS）──

    // 失败时按效果结清纪律处理：先重读单条判据，只有确证未生效才撤销；判据不可得
    // 时保留草稿及未决持久化事实，绝不删除。
    let frozen_bytes = FrozenSnapshotBytes::new(frozen_encoded);
    if let Err(error) = initialization.commit_frozen(&frozen_bytes).await {
        match committed_frozen_state(&resources, &session_id).await {
            Ok(true) => {
                warn!(
                    session_id = %session_id,
                    "frozen commit reported an error but the snapshot is present; publishing"
                );
            }
            Ok(false) => {
                drain_and_abandon(environment.as_ref(), &initialization).await?;
                return Err(super::super::workspace::resource_error(error));
            }
            Err(criterion) => {
                // 判据不可得：不得删除。环境仍按「未发布」排空（尽力），草稿连同
                // 未结清的执行代际留给显式恢复。
                if let Some(environment) = &environment {
                    let _ = environment.shutdown().await;
                }
                return Err(AcpError::new(
                    -32603,
                    format!(
                        "frozen commit failed and its outcome cannot be proven ({}); \
                         the unpublished session was kept for explicit recovery",
                        criterion.message
                    ),
                ));
            }
        }
    }

    super::resource_owners::bind(cfg, &session_id, &prepared.session_mcp_servers).await?;

    // ── P6 之前的同一任务内登记（非对外可见点）──
    // 通过 SessionManager 统一构造路径，并登记 AcpSession 记录以支撑
    // cascade cancel 子 agent 与 goal_state（见 SessionManager::ensure_session）。
    // frozen 与准备阶段同源：内容与字节都来自同一 PreparedSessionInputs
    // （日期/运行环境/配置/插件 roots 均为准备阶段定格的那一份），不二次构建；
    // 保存字节已由 P5 的 commit 一次写出，因此这里不存在「发布后需补偿」的中间态。
    cfg.session_manager.ensure_session_with_task_manager(
        &session_id,
        &cwd,
        environment.as_ref().map(|env| env.task_manager()),
    );

    // Create session-scoped WorkflowMiddleware at session/new (GAP-05: inject frozen data)
    let workflow_middleware =
        create_session_workflow_middleware(cfg, &cwd, &session_id, &frozen_data);

    // ── P6：发布（`sessions.insert` 是唯一对外可见点）──
    // 发布之后才可能有 prompt/执行；activate 已在 P3
    // 完成，因此这里的两次动作之间没有 await，不存在「已可见但资源未起」的窗口。
    sessions.insert(
        session_id.clone(),
        SessionState {
            session_id: session_id.clone(),
            thread_id: thread_id.clone(),
            cwd: cwd.clone(),
            environment: environment.clone(),
            closing: false,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: Some(frozen_data),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware,
            title: None,
            tags: Vec::new(),
        },
    );

    info!(session_id = %session_id, "ACP session created with ThreadStore");
    let modes = build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = NewSessionResponse::new(SessionId::new(&*session_id))
        .modes(modes)
        .config_options(config_options);
    // 将暂存的 peri caps 关联到新 session（MpscTransport 路径：若未
    // 显式调用 initialize（TUI 内部连接），默认全部 cap=true）。首次
    // AvailableCommandsUpdate 必须由 host 在 session/new response 成功发送后
    // 推送，确保客户端已能按 response 中的 sessionId 建立通知路由。
    cfg.session_manager.ensure_session_caps(&session_id);

    // BRIDGE_RESET_COUNTER handles stale committed cleanup; no explicit clear needed
    identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        identity,
    )
}

/// P5 失败后的效果结清：单条判据 `threads.frozen_context`。
///
/// `Ok(true)` = 非 NULL ⇒ 提交实际生效（转发布）；`Ok(false)` = NULL 且读得到
/// ⇒ 确证未提交（可撤销）；`Err` = 判据不可得 ⇒ 不得删除。
async fn committed_frozen_state(
    resources: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    session_id: &str,
) -> Result<bool, AcpError> {
    let snapshot = resources
        .load_session_snapshot(&session_id.to_owned())
        .await
        .map_err(super::super::workspace::resource_error)?;
    match snapshot.frozen {
        FrozenState::Present(_) => Ok(true),
        FrozenState::LegacyAbsent => Ok(false),
        // 有字节但本构建读不懂：判据存在但不能证明「已提交的是本次内容」，
        // 按不可得处理（不删除、不冒充发布）。
        FrozenState::Unsupported => Err(AcpError::new(
            -32603,
            "session frozen snapshot is present but not readable by this build",
        )),
    }
}

/// 发布前失败的补偿：环境先排空，**排空确认之后**才撤销未发布的创建。
///
/// 顺序不变量（§5.1）：`abandon` 必须在环境 drain 完成之后——否则资源持有的是已删除
/// 会话的资源句柄；排空未确认时不撤销（草稿与未决持久化保留）。
pub(super) async fn drain_and_abandon(
    environment: Option<&Arc<super::super::workspace::SessionEnvironment>>,
    initialization: &Arc<dyn SessionInitialization>,
) -> Result<(), AcpError> {
    if let Some(environment) = environment {
        if !environment.shutdown().await {
            return Err(AcpError::new(
                -32603,
                "session resources did not confirm shutdown; the unpublished session was kept",
            ));
        }
    }
    initialization
        .clone()
        .abandon()
        .await
        .map_err(super::super::workspace::resource_error)
}

/// `session/new` response 成功写入 transport 后执行的初始化通知。///
/// commands 首发与 MCP 预热必须保持此顺序：先挂载命令注册表的 on_change
/// 回调并发送 snapshot，再启动 MCP 发现，避免发现结果抢在首次 snapshot 前推送。
pub(crate) async fn after_new_response(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
) {
    let peri_caps = cfg.session_manager.ensure_session_caps(session_id);
    send_available_commands_update(
        transport,
        session_id,
        &peri_caps,
        cfg.session_manager.command_registry_for(session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, session_id);
}

// 曾有的「会话存储登记」两条 RPC（`peri/session_store_status` /
// `peri/session_register_store`）按用户裁决撤销：不再有本机登记、准入裁决与跨安装
// 来源判定，配置里指到哪个 store 就直接用哪个。历史见
// `docs/design/peri-acp-protocol.md`。

pub(crate) async fn handle_list(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    if let Some(extension) = params
        .get("_meta")
        .and_then(|meta| meta.get("peri.sessionWorkspaceV1"))
    {
        if !cfg
            .session_manager
            .effective_host_caps()
            .session_workspace_v1
        {
            return Err(AcpError::new(
                -32602,
                "Session workspace capability was not negotiated",
            ));
        }
        let scope = serde_json::from_value(
            extension
                .get("scope")
                .cloned()
                .ok_or_else(|| AcpError::new(-32602, "missing scope"))?,
        )
        .map_err(|e| AcpError::new(-32602, format!("Invalid scope: {e}")))?;
        let cursor = extension
            .get("cursor")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| AcpError::new(-32602, format!("Invalid cursor: {e}")))?;
        let limit = extension
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as u32;
        let query = peri_acp_types::workspace::ScopedThreadQuery {
            scope,
            cursor,
            limit,
        };
        let page = if extension.get("archived").and_then(Value::as_bool) == Some(true) {
            cfg.session_resources.list_archived_sessions(&query).await
        } else {
            cfg.session_resources.list_sessions(&query).await
        }
        .map_err(super::super::workspace::resource_error)?;
        let entries = page
            .entries
            .iter()
            .map(|entry| {
                agent_client_protocol::schema::v1::SessionInfo::new(
                    SessionId::new(entry.thread.id.clone()),
                    entry.effective_cwd.clone(),
                )
                .title(entry.thread.title.clone())
            })
            .collect::<Vec<_>>();
        let mut response = serde_json::to_value(ListSessionsResponse::new(entries))
            .map_err(super::super::workspace::workspace_error)?;
        response["_meta"]["peri.sessionWorkspaceV1"] =
            serde_json::json!({ "threads": page.entries, "nextCursor": page.next_cursor });
        return Ok(response);
    }
    let cwd_filter = params.get("cwd").and_then(|v| v.as_str());
    let entries = dispatch::list_sessions_as_info(cfg.controller.as_ref(), cwd_filter)
        .await
        .map_err(|e| AcpError::new(-32603, format!("Failed to list sessions: {e}")))?;

    let resp = ListSessionsResponse::new(entries);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(super) async fn handle_cancel_bg_task(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let task_id = params
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing taskId"))?;

    // 会话不存在时如实报错（此前静默返回 success，掩盖取消未生效）
    let session = cfg
        .session_manager
        .get_session(req_session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;
    let manager = Arc::clone(&session.task_manager);
    drop(session);
    manager
        .cancel_async(task_id)
        .await
        .map_err(|e| AcpError::new(-32603, e.to_string()))?;
    info!(session_id = %req_session_id, task_id = %task_id, "Background task cancelled via ACP");
    Ok(serde_json::json!({ "success": true }))
}

pub(super) fn handle_bg_tasks(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let session = cfg
        .session_manager
        .get_session(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    Ok(super::task_snapshot_value(session.task_manager.snapshot()))
}

#[path = "session_close.rs"]
mod close;
pub(super) use close::close_session;

pub(crate) async fn handle_close(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    close_session(cfg, sessions, id, false).await?;
    serde_json::to_value(CloseSessionResponse::new())
        .map_err(super::super::workspace::workspace_error)
}

pub(crate) async fn handle_delete(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    close_session(cfg, sessions, id, true).await?;
    serde_json::to_value(DeleteSessionResponse::new())
        .map_err(super::super::workspace::workspace_error)
}

pub(crate) async fn handle_fork(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let source_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    // prepare_existing 已在本准入里完整复核过源会话，这里只复核已记录证据。
    prepare_existing(params, cfg, sessions).await?;
    let workspace = super::super::workspace::reassert_expected(
        cfg,
        source_id,
        params.get("cwd").and_then(Value::as_str),
    )
    .await?;
    let source = sessions
        .get(source_id)
        .ok_or_else(|| AcpError::new(-32602, "Load the source session before forking"))?;
    let source_control = cfg
        .session_resources
        .load_session_control(&source_id.to_owned())
        .await
        .map_err(super::super::workspace::resource_error)?;
    if source_control.attempt.is_some()
        || source.cancel_token.is_some()
        || cfg
            .session_manager
            .get_session(source_id)
            .is_some_and(|s| !s.active_agents.is_empty() || !s.task_manager.is_execution_idle())
    {
        return Err(AcpError::new(
            -32010,
            "Cannot fork while source execution is active",
        ));
    }
    if source.frozen.is_none() {
        return Err(AcpError::new(-32603, "Source frozen snapshot is missing"));
    }
    let cwd_owned = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    let cwd = cwd_owned.as_str();
    // 一致 source 快照：payload/flags/binding/frozen 一次读出，ID 重映射是领域纯函数，
    // 目标快照由门面一次保存（不逐条写 flags、不做存储补偿）。
    let fork_source = dispatch::load_fork_source(&cfg.session_resources, source_id)
        .await
        .map_err(fork_source_error)?;
    // 普通 fork 复用 source 已持久化的精确 frozen 字节（内存对象只是同一次保存的
    // 解码视图），不按当前日期/目录重冻。frozen 与本次保存同源：字节来自 source
    // 快照，装配消费 `prepared` 的解码视图，不二次构建。
    let mut prepared = super::super::prepared::PreparedSessionInputs::prepare_fork(
        cfg,
        cwd,
        fork_source.frozen.as_str(),
    )?;
    prepared.session_mcp_servers = session_mcp_servers(params)?;
    let frozen_data = prepared
        .frozen
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "Fork frozen snapshot is missing"))?;
    let (new_thread_id, copied_payloads) = dispatch::fork_bound_session(
        &cfg.session_resources,
        &fork_source,
        &workspace,
        peri_time::now_utc_rfc3339(),
    )
    .await
    .map_err(fork_source_error)?;
    super::resource_owners::bind(cfg, &new_thread_id, &prepared.session_mcp_servers).await?;
    let identity = match response_identity(cfg, &new_thread_id).await {
        Ok(identity) => identity,
        Err(error) => {
            // identity 装配失败：环境尚未建立，撤销本次未发布的创建即可。
            cfg.session_resources
                .abandon_initialization(&new_thread_id)
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    let environment = match super::super::workspace::SessionEnvironment::assemble_prepared(
        cfg,
        &prepared,
        &new_thread_id,
    )
    .await
    {
        Ok(environment) => environment,
        Err(error) => {
            // 装配失败时环境尚未建立（没有对外资源需要排空）：撤销未发布的创建。
            cfg.session_resources
                .abandon_initialization(&new_thread_id)
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);

    let new_session_id = new_thread_id.clone();
    cfg.session_manager.ensure_session_with_task_manager(
        &new_session_id,
        cwd,
        environment.as_ref().map(|env| env.task_manager()),
    );
    let caps = cfg.session_manager.ensure_session_caps(&new_session_id);
    let workflow_middleware =
        create_session_workflow_middleware(cfg, cwd, &new_session_id, &frozen_data);

    sessions.insert(
        new_session_id.clone(),
        SessionState {
            session_id: new_session_id.clone(),
            thread_id: new_thread_id.clone(),
            cwd: cwd.to_string(),
            environment: environment.clone(),
            closing: false,
            history: copied_payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect(),
            history_payloads: copied_payloads,
            cancel_token: None,
            frozen: Some(frozen_data),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware,
            title: None,
            tags: Vec::new(),
        },
    );

    if let Some(environment) = &environment {
        environment.activate();
    }
    info!(source = %source_id, new = %new_session_id, "Session forked");
    // Push AvailableCommandsUpdate notification + 预热 MCP skill 发现
    // （决策 B 扩展，与 session/new 同构；stdio 装配面同款行为——fork 产生
    // 新 session 后无需等首 turn before_agent 装配即有 mcp 命令）。
    send_available_commands_update(
        transport,
        &new_session_id,
        &caps,
        cfg.session_manager.command_registry_for(&new_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, &new_session_id);
    let resp = ForkSessionResponse::new(SessionId::new(new_session_id.clone()));
    identity_response(
        serde_json::to_value(resp).map_err(super::super::workspace::workspace_error)?,
        identity,
    )
}

pub(super) async fn handle_rename(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing title"))?;

    // 定向更新：只改标题，不整份覆盖 metadata（cwd/binding/计数/缓存的持有者是门面）。
    cfg.session_resources
        .update_session_meta(
            &session_id.to_owned(),
            &SessionMetaPatch {
                title: Some(Some(title.to_owned())),
                ..Default::default()
            },
        )
        .await
        .map_err(super::super::workspace::resource_error)?;

    // 通过 session/update 通知推送新的标题给外部客户端
    super::super::notify::send_session_info_update_with_title(
        transport.as_ref(),
        session_id,
        Some(title),
    )
    .await;

    info!(session_id = %session_id, title = %title, "Session renamed");

    Ok(serde_json::json!({
        "sessionId": session_id,
        "title": title,
    }))
}

/// 新会话 MCP skill 发现预热（决策 B 扩展）：session/new 完成时挂接连接
/// 事件 notifier + 触发幂等发现，chain 首 turn 装配前即可开始。任何组件
/// 缺失（pool 未装配 / registry 缺失）→ 空跑返回，由首 turn 装配兜底；
/// cancel 持 session token，会话关闭即早退。notifier 无 ExecutorEvent 通道
/// （通知展示由首 turn 装配覆盖为完整版），连接完成事件在此即触发发现。
pub(super) fn prewarm_session_mcp_discovery(cfg: &AcpServerConfig, session_id: &str) {
    let Some(pool) = cfg.mcp_pool.clone() else {
        return;
    };
    let Some(registry) = cfg.session_manager.mcp_skill_registry_for(session_id) else {
        return;
    };
    let Some(command_registry) = cfg.session_manager.command_registry_for(session_id) else {
        return;
    };
    let Some(cancel) = cfg
        .session_manager
        .inner_sessions()
        .get(session_id)
        .map(|s| s.cancel_token.clone())
    else {
        return;
    };
    pool.clone()
        .attach_connection_notifier(Some(&registry), Some(&command_registry), &cancel);
    pool.prewarm_discovery(&registry, &command_registry, session_id, &cancel);
}
