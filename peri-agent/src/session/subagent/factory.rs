use std::sync::Arc;

use peri_acp_types::identity::AgentId;
use peri_acp_types::thread::CancelPolicy;
use tokio_util::sync::CancellationToken;

use super::background::spawn_background_subagent;
use super::directives::{build_bg_fork_directive, build_fork_directive};
use super::run_sync::run_sync_subagent;
use super::types::{
    ForkDirectiveKind, SubagentCancelPolicy, SubagentChainAssembler, SubagentChainContext,
    SubagentResumeConfig, SubagentRunMode, SubagentSpawnConfig, SubagentSpawned,
};
use super::v2_bridge::{agent_id_from_child_thread, build_v2_subagent_context, V2SubagentContext};
use crate::agent::react::ReactLLM;
use crate::agent::{CompactConfig, ContextBudget};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::messages::BaseMessage;
use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};
use crate::session::{FrozenContext, MessageQueue, Session};
use crate::thread::{ThreadMeta, ThreadStore};
use crate::tools::{BaseTool, ToolInvocationResolver};

// ─── 统一入口 ────────────────────────────────────────────────────────────────

/// Agent 层 session 工厂（L3）：subagent 创建统一入口命名空间。
///
/// 验收契约（子 issue L3）：`SessionFactory::spawn_subagent(parent, config)`
/// 为唯一 subagent 创建入口，位于 peri-agent。Middleware 只组装
/// [`SubagentSpawnConfig`] 发起意图，不持有创建实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionFactory;

impl SessionFactory {
    /// 启动子 agent（唯一创建入口，见 [`spawn_subagent_impl`] 的流程说明）。
    pub async fn spawn_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        spawn_subagent_impl(parent, config).await
    }

    /// 恢复子 agent（唯一恢复入口，见 [`resume_subagent_impl`] 的校验流程说明）。
    ///
    /// 主 agent 凭中断/错误/bg 通知文本携带的 `child_thread_id` 重新唤起被中断的
    /// subagent：从磁盘 thread_store 加载 meta 校验（存在 / 非 active）后重建现场
    /// 继续执行。thread_id 不变，可无限次恢复。
    pub async fn resume_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentResumeConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        resume_subagent_impl(parent, config).await
    }
}

/// 父线程 ID 解析——spawn 写盘的**唯一取值点**（挂父子链）：
/// - 优先 parent session 的 `store().thread_id`：subagent 层 session 构造时以
///   child_thread_id 注入，恒为 `Some`（孙 agent 链命中此值）；
/// - 回退 `SubagentHost.parent_thread_id`：TUI 主 agent 的 `store().thread_id`
///   恒为 `None`（stage_builder 构造主 session 不传 thread_id，`SessionStore`
///   无 setter），executor 以 `ctx.thread_id` 注入 host
///   （`ThreadPersistence.parent_thread_id` → stage_builder → host）。
///
/// parent 为 `None` 时返回 `None`：spawn 侧继续走 `parent_thread_id_cfg` 回退。
/// （resume 路径不再做 parent 链校验——该解析链路在生产路径与写盘值常有
/// 偏差，误判拒绝；resume 仅以 thread_id 存在性 / status 为准）
fn parent_thread_id_of(parent: Option<&Arc<Session>>) -> Option<String> {
    parent
        .and_then(|p| p.store().thread_id.clone())
        .or_else(|| parent.and_then(|p| p.subagent_host().and_then(|h| h.parent_thread_id.clone())))
}

/// 启动子 agent（统一创建入口实现，L3）。
///
/// 流程（与迁移前四条路径语义一致）：
/// 1. 生成 child_thread_id / task_id
/// 2. 解析父侧数据（parent 优先；frozen copy 自 parent session，不重读磁盘）
/// 3. 创建子线程（thread_store Some 时；parent_thread_id 挂父子链）
/// 4. 构造子 session（frozen copy + transcript with_persistence 绑定存储）
/// 5. 注入 parent_messages / system_prompt 到 transcript，push prompt 到 queue
/// 6. 经 chain_assembler 装配子链（frozen 注入链上下文），构造 StageContext
/// 7. Sync：直接 run_react_loop；Background：tokio::spawn + TaskManager 注册
/// 8. 收尾：update_thread_status（done/cancelled/error）+ 事件 + hook 闭包
///
/// 并发限制（Background 最多 3 个活跃任务）：不做入口预检，由注册阶段的
/// `register_with_kind`（per-kind 上限）如实返回注册失败——与迁移前一致，
/// 预检（若有）位于调用方（llm_factory 之前），保证「预检 → 装配 → 注册」
/// 的确定性窗口不被重复预检破坏（S3.1 幽灵任务回归测试依赖此结构）。
#[allow(clippy::too_many_arguments)]
async fn spawn_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentSpawnConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config：字段分散使用，避免部分 move 后整体借用冲突
    let SubagentSpawnConfig {
        agent_name,
        prompt,
        parent_messages,
        cancel_policy,
        max_iterations,
        fork_directive_kind,
        run_mode,
        skill_names,
        llm,
        chain_assembler,
        tools,
        tool_filter,
        system_prompt,
        error_suggest_registry,
        tool_registry_snapshot,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        thread_store,
        event_handler,
        bg_event_sender,
        task_manager,
        on_bg_complete,
        langfuse_bridge,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        deregister_runtime,
        parent_agent_id,
        cancel_token: cancel_token_cfg,
        cwd: cwd_cfg,
        parent_thread_id: parent_thread_id_cfg,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    // 并发限制由注册阶段兜底（register_with_kind per-kind 上限，错误如实返回），
    // 不在入口预检：middlewares 路径的预检位于 llm_factory 之前（execute_bg.rs），
    // 保证并发竞态窗口内错误语义与迁移前一致（"Failed to register"，S3.1）。

    // 2. 生成标识符
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    let task_id = format!("bg-{}", uuid::Uuid::now_v7());

    // 3. 父侧数据解析（parent 优先；frozen data 从父 session copy）
    let cwd = parent
        .map(|p| p.store().cwd.to_string())
        .or(cwd_cfg)
        .ok_or("spawn_subagent: cwd 未提供（parent 缺失且 config.cwd 为 None）")?;
    let parent_thread_id = parent_thread_id_of(parent).or(parent_thread_id_cfg);
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    let frozen_claude_local_md = frozen_claude_local_md_cfg;

    // cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    // 父 token；均缺失时新建），Independent = 新建（与迁移前语义一致）
    let cancel_token = match cancel_policy {
        SubagentCancelPolicy::Cascade => parent
            .map(|p| p.config().cancel_token.child_token())
            .or_else(|| cancel_token_cfg.map(|t| t.child_token()))
            .unwrap_or_default(),
        SubagentCancelPolicy::Independent => CancellationToken::new(),
    };
    let cancel_policy = cancel_policy.as_cancel_policy();

    // 4. 创建子线程（thread_store Some 时；None 跳过落库——仅测试/遗留路径）
    if let Some(ref store) = thread_store {
        let snapshot_id = parent_messages.last().map(|m| m.id().as_uuid().to_string());
        let mut child_meta = ThreadMeta::new(&cwd);
        child_meta.id = child_thread_id.clone();
        child_meta.parent_thread_id = parent_thread_id.clone();
        child_meta.snapshot_at_message_id = snapshot_id;
        child_meta.hidden = true;
        child_meta.cancel_policy = cancel_policy;
        child_meta.title = Some(agent_name.clone());
        store
            .create_thread(child_meta)
            .await
            .map_err(|e| format!("Failed to create child thread: {}", e))?;
    }

    // 5. 构造子 session + 链装配 + v2_ctx（共享 helper [build_subagent_session_v2]：
    //    frozen 从父 copy 不重读磁盘，transcript 绑定存储，ancestor 为空）
    //    注入 parent_messages / system_prompt / prompt 留在本函数——spawn 与
    //    resume 的消息注入差异大，不进 helper（D1）
    let frozen = FrozenContext {
        system_prompt: parent
            .map(|p| Arc::clone(&p.store().frozen.system_prompt))
            .unwrap_or_default(),
        claude_md: frozen_claude_md
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        skill_summary: frozen_skill_summary
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        date: frozen_date
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        language: parent.and_then(|p| p.store().frozen.language.clone()),
        // MetaHarness 冻结状态随父 session 复制（ARC-FROZEN-001：不重读配置/磁盘）
        meta_harness: parent
            .map(|p| p.store().frozen.meta_harness.clone())
            .unwrap_or_default(),
    };
    let (session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        child_thread_id.clone(),
        thread_store.clone(),
        Vec::new(), // 无 ancestor（spawn 新建 thread，transcript 为空）
        llm,
        chain_assembler,
        tools,
        tool_filter,
        parent
            .and_then(|session| session.subagent_host())
            .and_then(|host| host.session_mcp_capability.clone()),
        skill_names,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&child_thread_id)),
    );

    let transcript = session.transcript();

    // 6a. fork 路径：把 parent_messages 注入 transcript（让子 agent 看到父会话上下文）
    if !parent_messages.is_empty() {
        let mut tx = transcript.write();
        for msg in &parent_messages {
            tx.append(msg.clone());
        }
    }

    // 6b. SubAgent system_prompt（身份构建）注入到 transcript 开头位置：
    // - fork 路径：在 parent_messages 之后（让身份提示词位于对话上下文之后、
    //   prompt 之前——SubAgent 的 prompt 由下方 push 到 queue，Receive 阶段追加）
    // - 非 fork 路径：parent_messages 为空，直接 append 到 transcript 开头
    //
    // 注意：这是 session 起始身份构建（在 run_react_loop 调用前注入），不是中途纠正，
    // 用 BaseMessage::System 合法（CLAUDE.md TRAP 仅禁止中途纠正用 System）。
    if let Some(sp) = system_prompt {
        let mut tx = transcript.write();
        tx.append(BaseMessage::system(sp));
    }

    // 6c. push prompt 到 queue（fork 路径套 fork directive 模板）
    let prompt_message = match fork_directive_kind {
        Some(ForkDirectiveKind::Fork) => build_fork_directive(&prompt),
        Some(ForkDirectiveKind::Bg) => build_bg_fork_directive(&prompt),
        None => prompt.clone(),
    };
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_message),
    ));

    match run_mode {
        SubagentRunMode::Sync => {
            let interrupted = run_sync_subagent(
                &child_thread_id,
                &agent_name,
                &cwd,
                max_iterations,
                event_handler,
                on_subagent_start,
                on_subagent_stop,
                thread_store,
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
                v2_ctx,
                session.clone(),
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id,
                task_id: None,
                session,
                cancel_token,
                interrupted,
            })
        }
        SubagentRunMode::Background => {
            let task_id_clone = task_id.clone();
            spawn_background_subagent(
                task_id.clone(),
                child_thread_id.clone(),
                agent_name.clone(),
                prompt,
                cwd.clone(),
                max_iterations,
                bg_event_sender,
                task_manager,
                on_bg_complete,
                langfuse_bridge,
                thread_store,
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                cancel_token.clone(),
                v2_ctx,
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id,
                task_id: Some(task_id_clone),
                session,
                cancel_token,
                interrupted: false,
            })
        }
    }
}

// ─── 共享 session 构造（spawn / resume 共用，D1） ───────────────────────────

/// 构造子 session + 链装配 + v2_ctx（[`spawn_subagent_impl`] 与
/// [`resume_subagent_impl`] 共用的装配块，纯 move 提取——spawn 行为不变）。
///
/// - session 以 `child_thread_id` 为 thread_id（subagent 必有持久化 thread；
///   thread_id = agent_id）；
/// - transcript 装载 `ancestor`（resume 的旧 transcript 重放；spawn 传空——
///   `with_ancestor(vec![])` 为 no-op），再 `with_persistence` 绑定存储
///   （**顺序不可反**：with_ancestor 只建 id_index、不触发持久化
///   transcript.rs:158-169，append 会 send_persist 二次落库 :430-438）；
/// - 链装配（skill_names / frozen 注入链上下文；链序由 assembler 实现方保持）；
/// - `build_v2_subagent_context` 构造 StageContext。
///
/// 父侧解析 / cancel token / 消息注入（parent_messages / system_prompt /
/// prompt）差异大，留在调用方（D1）。
#[allow(clippy::too_many_arguments)]
fn build_subagent_session_v2(
    cwd: String,
    frozen: FrozenContext,
    cancel_token: CancellationToken,
    child_thread_id: String,
    thread_store: Option<Arc<dyn ThreadStore>>,
    ancestor: Vec<BaseMessage>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain_assembler: Arc<dyn SubagentChainAssembler>,
    tools: Vec<Arc<dyn BaseTool>>,
    tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    session_mcp_capability: Option<Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort>>,
    skill_names: Vec<String>,
    frozen_claude_md: Option<String>,
    frozen_claude_local_md: Option<String>,
    frozen_skill_summary: Option<String>,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> (Arc<Session>, V2SubagentContext) {
    let cancel_arc: Arc<CancellationToken> = Arc::new(cancel_token.clone());
    // SubAgent 独立 MessageQueue（不与 main agent 共享）
    let queue = MessageQueue::new();
    let session = Session::new_with_cancel_and_queue(
        Arc::from(cwd.as_str()),
        frozen,
        Some(child_thread_id.clone()),
        cancel_arc,
        queue,
    );

    // transcript 绑定（ancestor 先于 with_persistence，顺序不可反）
    {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        let with_ancestor = old.with_ancestor(ancestor);
        *transcript = match thread_store {
            Some(ref store) => {
                with_ancestor.with_persistence(Arc::clone(store), child_thread_id.clone())
            }
            None => with_ancestor,
        };
    }

    // 子链装配（frozen 数据注入链上下文；链序由 assembler 实现方保持）。
    // meta_harness_disabled 从 frozen 状态投影（spawn/resume 两条路径都复制
    // 父 meta_harness，子链独立装配必须同样过滤——设计 §2.5）。
    let chain = chain_assembler.assemble(&SubagentChainContext {
        cwd: cwd.clone(),
        skill_names,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        meta_harness_disabled: session
            .store()
            .frozen
            .meta_harness
            .disabled_middlewares
            .clone(),
    });

    // StageContext 构造（v2_bridge 迁移；tool_invocation_resolver 参数化；
    // 复用上面预创建的 session——transcript 已装载 ancestor 并绑定持久化）
    let v2_ctx = build_v2_subagent_context(
        Some(session.clone()),
        llm,
        chain,
        tools,
        tool_filter,
        session_mcp_capability,
        &cwd,
        cancel_token,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        agent_id,
    );

    (session, v2_ctx)
}

// ─── 恢复（统一入口 resume_subagent） ───────────────────────────────────────

/// resume 校验段互斥锁（R2-LOW-2）：static 全局单锁（`SessionFactory` 为 unit
/// struct 无字段，锁表不能放 factory；放工具实例则多实例互斥失效）。
///
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，tokio::sync::Mutex 跨 await
/// 安全，无死锁）。resume 为低频操作，单锁即可。跨进程双 resume 仍可能双执行，
/// 属已接受限制（缓解定位，与 issue 非目标「不自动恢复」一致）。
static RESUME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 恢复子 agent（统一入口实现，slice 4：重建 + 执行）。
///
/// 两层校验（不通过返回明确 Err，与 issue 验收一致）：
/// 1. 存在性：`load_meta` 失败/不存在 → `thread not found`
/// 2. status：`agent_status == Active`（可能未正常收尾）→ 拒绝恢复
///
/// parent 链校验已移除：parent_thread_id 解析链路（store().thread_id 优先、
/// 回退 host 注入值）在生产路径经常与写盘值不一致，误判导致兄弟 subagent
/// 无法恢复；thread_id 本身就是不透明凭证，持有 child_thread_id 即可恢复。
///
/// 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）；
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，不 await run_react_loop）。
/// 锁释放后重建：
/// - `load_messages` 重放 transcript；**仅当末条**含未配对 tool_calls 的 AI 时
///   pop（R2-MID-1：禁止从后往前找 AI；pop 后其后无消息，无孤儿 Tool 可清理）
/// - cwd 取 `meta.cwd`（thread 创建时固化，进程重启后不得改用父 cwd）
/// - frozen 从父 session copy（ARC-FROZEN-001；parent None 用 config 回退）
/// - cancel token：meta.cancel_policy == Cascade 且 parent 存在 → 从父重新派生
///   （重启后父是新 token 树）；否则用 config 注入的 token（缺省新建）
/// - **不注入** parent_messages / identity System / skill_names（F4 / R-H1：
///   旧 transcript 已含首轮注入内容，重复注入会重复）
/// - prompt 入队：`Some(p)` 原样追加（不套 fork directive）；`None` 注入隐式
///   continue 常量（issue 决策 9）
/// - run mode 由本次调用决定（issue 决策 8）：Sync → `run_sync_subagent`；
///   Background → 新 task_id + `spawn_background_subagent`
///
/// 重建/装配失败（load_messages 失败）时回滚 status 至原值（R-M1），防 thread
/// 永久停留 active（R-M4 崩溃遗留的镜像问题）。执行开始后的失败走
/// `run_sync_subagent` / bg 各自的收尾路径（error / cancelled），不回滚。
async fn resume_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentResumeConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config（cwd 不用于恢复——cwd 取 meta.cwd，thread 创建时固化）
    let SubagentResumeConfig {
        thread_id,
        prompt,
        agent_name: agent_name_cfg,
        run_mode,
        max_iterations,
        llm,
        chain_assembler,
        tools,
        tool_filter,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        thread_store,
        event_handler,
        bg_event_sender,
        task_manager,
        on_bg_complete,
        langfuse_bridge,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        deregister_runtime,
        parent_agent_id,
        cancel_token: cancel_token_cfg,
        cwd: _,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    // 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）
    let guard = RESUME_LOCK.lock().await;

    // 0. thread_id 格式校验（review low-1）：重建阶段 `agent_id_from_child_thread`
    //    会对非 UUID 字符串 expect panic，入口统一拒绝
    if uuid::Uuid::parse_str(&thread_id).is_err() {
        return Err(format!("resume_subagent: invalid thread id: {}", thread_id).into());
    }

    // 1. 存在性校验（load_meta 失败/不存在统一映射为 not found）
    let meta = thread_store
        .load_meta(&thread_id)
        .await
        .map_err(|_| format!("resume_subagent: thread not found: {}", thread_id))?;

    // 2. status 校验（R-M4：active = 未正常收尾，崩溃遗留需手动处理）
    if meta.agent_status.is_active() {
        return Err(format!(
            "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
            thread_id
        )
        .into());
    }
    // 原值快照：重建失败时回滚（R-M1）
    let previous_status = meta.agent_status;

    // 校验通过 → 锁内立即置 active（R-M1：置 active 与校验原子化，第二个并发
    // resume 在锁内看到 active 被拒）；重建失败时下方回滚
    thread_store
        .update_thread_status(&thread_id, "active")
        .await
        .map_err(|e| {
            format!(
                "resume_subagent: failed to mark thread {} active: {}",
                thread_id, e
            )
        })?;

    // 释放锁：重建/执行不持锁（load_messages 与 run_react_loop 不在互斥段内）
    drop(guard);

    // ── 重建（失败回滚 status 至原值，防 thread 永久停留 active）──

    // 1. 加载 transcript；末条含未配对 tool_calls 的 AI 则 pop（R2-MID-1：
    //    仅末条规则，幂等——磁盘旧消息不删除，每次 resume 重截）
    let mut loaded = match thread_store.load_messages(&thread_id).await {
        Ok(msgs) => msgs,
        Err(e) => {
            // R-M1 回滚：重建失败 → status 回滚至原值（不置 active 卡死）
            let _ = thread_store
                .update_thread_status(&thread_id, previous_status.as_str())
                .await;
            return Err(format!(
                "resume_subagent: failed to load messages for {}: {}",
                thread_id, e
            )
            .into());
        }
    };
    if loaded.last().is_some_and(|m| m.has_tool_calls()) {
        loaded.pop();
    }

    // 2. cwd 取 meta.cwd（thread 创建时固化的；进程重启后不得改用父 cwd）
    let cwd = meta.cwd.clone();

    // 3. frozen 从父 session copy（ARC-FROZEN-001：不重读磁盘；parent None 用
    //    config 回退，与 spawn 的父侧解析一致）
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    let frozen = FrozenContext {
        system_prompt: parent
            .map(|p| Arc::clone(&p.store().frozen.system_prompt))
            .unwrap_or_default(),
        claude_md: frozen_claude_md
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        skill_summary: frozen_skill_summary
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        date: frozen_date
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        language: parent.and_then(|p| p.store().frozen.language.clone()),
        // MetaHarness 冻结状态随父 session 复制（resume 路径，ARC-FROZEN-001）
        meta_harness: parent
            .map(|p| p.store().frozen.meta_harness.clone())
            .unwrap_or_default(),
    };

    // 4. cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    //    父 token；均缺失时新建——与 spawn 的 :466-472 完全对齐，review low-2），
    //    Independent = 恒新建（忽略 config 注入的 token——复用会让父 cancel
    //    波及 Independent 子任务，与 spawn :471 对齐，review low-1）
    let cancel_token = if meta.cancel_policy == CancelPolicy::Cascade {
        parent
            .map(|p| p.config().cancel_token.child_token())
            .or_else(|| cancel_token_cfg.map(|t| t.child_token()))
            .unwrap_or_default()
    } else {
        CancellationToken::new()
    };

    // 5. agent_name：config 优先，回退 meta.title，最后兜底 "subagent"
    let agent_name = agent_name_cfg
        .or_else(|| meta.title.clone())
        .unwrap_or_else(|| "subagent".to_string());

    // 6. 重建 session（thread_id 固定 = config.thread_id；with_ancestor 装载
    //    旧 transcript 重放 + with_persistence 绑定，顺序不可反——helper 内）
    //    不注入 parent_messages / identity System / skill_names（F4 / R-H1）
    let (session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        thread_id.clone(),
        Some(Arc::clone(&thread_store)),
        loaded,
        llm,
        chain_assembler,
        tools,
        tool_filter,
        parent
            .and_then(|session| session.subagent_host())
            .and_then(|host| host.session_mcp_capability.clone()),
        Vec::new(), // skill_names 恒空（R-H1：恢复不重复注入 SkillPreload）
        frozen_claude_md,
        frozen_claude_local_md_cfg,
        frozen_skill_summary,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&thread_id)),
    );

    // 7. prompt 入队：Some(p) 原样追加（不套 fork directive——恢复目标仍是原
    //    任务，直接追加指令）；None 注入隐式 continue 常量（issue 决策 9）
    let prompt_text = prompt.unwrap_or_else(|| IMPLICIT_CONTINUE_PROMPT.to_string());
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_text.clone()),
    ));

    // 8. 执行（run mode 由本次调用决定，issue 决策 8）
    match run_mode {
        SubagentRunMode::Sync => {
            let interrupted = run_sync_subagent(
                &thread_id,
                &agent_name,
                &cwd,
                max_iterations,
                event_handler,
                on_subagent_start,
                on_subagent_stop,
                Some(Arc::clone(&thread_store)),
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
                v2_ctx,
                session.clone(),
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id: thread_id,
                task_id: None,
                session,
                cancel_token,
                interrupted,
            })
        }
        SubagentRunMode::Background => {
            // slice 5：后台恢复——生成新 task_id、TaskManager 注册（参数与 spawn
            // 调用点对齐；prompt 传实际注入文本——用户 prompt 或 continue 常量，
            // 仅用于 prompt_summary 展示，R2-LOW-3）
            let task_id = format!("bg-{}", uuid::Uuid::now_v7());
            match spawn_background_subagent(
                task_id.clone(),
                thread_id.clone(),
                agent_name.clone(),
                prompt_text,
                cwd,
                max_iterations,
                bg_event_sender,
                task_manager,
                on_bg_complete,
                langfuse_bridge,
                Some(Arc::clone(&thread_store)),
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                cancel_token.clone(),
                v2_ctx,
            )
            .await
            {
                Ok(()) => {}
                Err(e) => {
                    // review MEDIUM-1 回滚：注册失败（task_manager 缺失 /
                    // register_with_kind 撞 per-kind 上限）时任务未执行——status
                    // 回滚至原值，防 thread 永久停留 active（R-M1 执行前失败回滚
                    // 契约，与 load_messages 失败回滚同款）；错误携带 thread_id
                    let _ = thread_store
                        .update_thread_status(&thread_id, previous_status.as_str())
                        .await;
                    return Err(format!("resume_subagent: thread {}: {}", thread_id, e).into());
                }
            }
            Ok(SubagentSpawned {
                child_thread_id: thread_id,
                task_id: Some(task_id),
                session,
                cancel_token,
                interrupted: false,
            })
        }
    }
}

/// 隐式 continue 指令（prompt 缺省时注入，issue 决策 9）
const IMPLICIT_CONTINUE_PROMPT: &str = "Continue your previous task where you left off.";
