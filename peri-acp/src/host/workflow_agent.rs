//! Workflow agent 装配面薄壳（p1-wa 收口）。
//!
//! 执行体已随 p1-wa 物理迁入 `peri_agent::agent::workflow`（`agent.rs` /
//! `factory.rs`——session 运行单元归 Agent 层，§2）；中间件链 / 工具 /
//! tool resolver / session 级 WorkflowMiddleware 装配经
//! [`WorkflowMiddlewareFactory`] 端口注入（peri-middlewares 实现，ACP 宿主
//! 装配点注入）。
//!
//! 本模块保留 ACP 装配面职责（§0 边 2：ACP 不再持有 middlewares/workflow
//! 引用——`scripts/import-exemptions.conf` 的 L5 豁免随本任务移除）：
//!
//! 1. `create_session_workflow_middleware`：session 级 WorkflowMiddleware
//!    装配编排（executor 构造 + progress channel + 端口装配）；
//! 2. 注入面构造 helpers（provider/peri_config 投影模型工厂、publish hook、
//!    forwarder launcher、system prompt fallback）——构造点收敛在本模块，
//!    防注入面漂移（`host/requests.rs` / `host/stdio` 装配面共用）。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::{
    agents::AgentOverrides,
    mcp_skills::McpSkillRegistry,
    ports::{AgentCatalogPort, WorkflowMiddlewarePort},
    workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult},
};
use peri_agent::agent::workflow::{
    create_executor, WorkflowAgentContext, WorkflowAgentPromptBuilder, WorkflowMiddlewareFactory,
    WorkflowModel, WorkflowModelFactory, WorkflowPublishHook, WorkflowSystemPromptFallback,
};
use peri_agent::session::exec::executor_helpers::ForwarderLauncherFn;

use crate::provider::{LlmProvider, PeriConfig};
use crate::session::executor::FrozenSessionData;

/// 模型工厂构造：provider / peri_config 投影。
///
/// provider 经 `Arc<RwLock<>>` 共享——provider/model 切换后自动感知，无需
/// 重建 executor（与迁移前 `WorkflowAgentContext.provider` 语义一致）；
/// retry observer 由执行体按 run 传入（重试观测翻译为 LlmRetrying 交给
/// 本 run handler）。
///
/// 池化分支（迁移前 `ctx.agent_pool`）：未文档化契约——与主 builder 的
/// `ctx.pool` 必须是同一 `Arc<Mutex<AgentPool>>`，池化模型烘焙的 observer
/// 才会与主链路共享同一转发器；迁移前 4 处入口均传 `agent_pool: None`
/// （死路径）。若未来接线，池化烘焙在本工厂内实现。
pub(crate) fn build_model_factory(
    provider: &Arc<RwLock<LlmProvider>>,
    peri_config: &RwLock<PeriConfig>,
) -> WorkflowModelFactory {
    let provider = Arc::clone(provider);
    let peri_config = Arc::new(peri_config.read().clone());
    Arc::new(
        move |model: Option<&str>, max_tokens: Option<u32>, observer| {
            // 合并 provider 读取为一次（display/model 同源，避免中间切换导致
            // 不一致——与迁移前 execute() 块作用域语义一致）。如 workflow 脚本
            // 指定了 model 参数：
            //   1) 有 PeriConfig → 尝试 alias 解析（haiku/sonnet/opus → 真实模型名）
            //   2) 解析失败或无 PeriConfig → 替换 provider 的 model name 按字面量使用
            // `tier` 仅在 alias 解析成功时有值（请求参数即档位名）。
            let (effective, tier) = {
                let provider_read = provider.read();
                match model {
                    Some(m) => match LlmProvider::from_config_for_alias(&peri_config, m) {
                        Some(p) => (p, Some(m.to_string())),
                        None => (provider_read.with_model_name(m.to_string()), None),
                    },
                    None => (provider_read.clone(), None),
                }
            };
            // `maxTokens` 是单次 workflow agent 调用的输出上限；提供时覆盖 profile，
            // 未提供时保留 profile/provider 的默认值。
            let effective = max_tokens
                .map(|max_tokens| effective.with_max_tokens(max_tokens))
                .unwrap_or(effective);
            let model_name = effective.model_name().to_string();
            WorkflowModel {
                model: Arc::from(effective.with_retry_observer(Some(observer)).into_model()),
                model_name,
                tier,
            }
        },
    )
}

/// 事件发射钩子构造（`Controller::publish_event` 适配；事件三层化统一出口，
/// workflow agent 的 v2 事件经此进入协议化路径，与主 executor 同一出口）。
pub(crate) fn build_publish_hook(
    controller: &Arc<peri_controller::Controller>,
) -> WorkflowPublishHook {
    let controller = Arc::clone(controller);
    Arc::new(move |sid: &str, source, ev| controller.publish_event(sid, source, ev.clone()))
}

/// EventBus forwarder 启动器构造（workflow 专用：bridge = None——workflow 的
/// Langfuse 处理在外部事件旁路处理器，与迁移前 `spawn_eventbus_forwarder`
/// 调用点一致；biased select 顺序不变量单点保持在 `crate::event`）。
pub(crate) fn build_workflow_forwarder_launcher() -> ForwarderLauncherFn {
    Arc::new(|handles, _agent_id, on_event| {
        crate::event::spawn_eventbus_forwarder(handles, on_event, None)
    })
}

/// system prompt fallback 渲染闭包构造（`PromptTemplate` 渲染面；skills 经
/// 注入的 [`AgentCatalogPort`] 访问——与宿主装配点注入的端口实现同一类型）。
///
/// 16_workflow 已删除（C2），workflow agent 渲染与主链共用同一段落来源；
/// `meta_harness` 为冻结期 MetaHarnessState（随调用点从 `FrozenSessionData`
/// 注入，段落覆盖与主会话同源——禁止重读配置，设计 §2.4）。
///
/// H2：段落集合来自 **workflow 链能力事实**（`capabilities`）——workflow 链
/// 没有子代理持有者，审批仅在 broker + permission_mode 齐备（有效模式）时
/// 声明；不再按执行类型硬编码过滤 10_hitl。H3：运行环境只消费冻结快照。
pub(crate) fn build_workflow_system_prompt_fallback(
    agent_catalog: Arc<dyn AgentCatalogPort>,
    meta_harness: peri_acp_types::meta_harness::MetaHarnessState,
    capabilities: peri_agent::middleware::SectionCapabilities,
    frozen_runtime_env: Option<peri_acp_types::frozen::FrozenRuntimeEnv>,
) -> WorkflowSystemPromptFallback {
    Arc::new(
        move |cwd: &str, frozen_date: Option<&str>, frozen_language: Option<&str>| {
            // C2：收集结果 = 能力事实驱动（冻结 disabled 集合 + 冻结语言）。
            let collected = crate::session::build_collected_sections_with_capabilities(
                &meta_harness,
                None,
                frozen_language,
                &capabilities,
            );
            let template = crate::prompt::PromptTemplate::new(&meta_harness, &collected);
            let date = frozen_date.map(str::to_string).unwrap_or_else(|| {
                peri_time::calendar_date(
                    peri_time::now_wall(),
                    peri_time::CalendarConvention::deployment_default(),
                )
                .to_string()
            });
            let env = crate::prompt::PromptEnv::frozen(cwd, &date, frozen_runtime_env.as_ref());
            template.render(&env, agent_catalog.as_ref())
        },
    )
}

/// workflow `agentType` 指定时的 subagent prompt 渲染器。
///
/// 与主链注入的 `system_builder` 使用相同的 PromptTemplate 语义；
/// 16_workflow 已删除（C2），无子面向 feature 差异。
///
/// `meta_harness` 为冻结期 MetaHarnessState（同源注入，见
/// `build_workflow_system_prompt_fallback`）；`capabilities` 为 workflow 链
/// 能力事实（H2，含权限有效模式），`frozen_runtime_env` 为冻结运行环境（H3）。
pub(crate) fn build_workflow_agent_prompt_builder(
    agent_catalog: Arc<dyn AgentCatalogPort>,
    meta_harness: peri_acp_types::meta_harness::MetaHarnessState,
    capabilities: peri_agent::middleware::SectionCapabilities,
    frozen_runtime_env: Option<peri_acp_types::frozen::FrozenRuntimeEnv>,
) -> WorkflowAgentPromptBuilder {
    Arc::new(
        move |overrides: Option<&AgentOverrides>, cwd, frozen_date, frozen_language| {
            // C2：收集结果 = 能力事实驱动（冻结 disabled 集合 + overrides +
            // 冻结语言驱动；persona 段内容依赖 overrides，调用期计算）。
            // H2：不再按执行类型硬编码排除 10_hitl——审批有效性由能力事实
            // （PermissionMiddleware 有效模式）决定。
            let collected = crate::session::build_collected_sections_with_capabilities(
                &meta_harness,
                overrides,
                frozen_language,
                &capabilities,
            );
            let template = crate::prompt::PromptTemplate::new(&meta_harness, &collected);
            let date = frozen_date.map(str::to_string).unwrap_or_else(|| {
                peri_time::calendar_date(
                    peri_time::now_wall(),
                    peri_time::CalendarConvention::deployment_default(),
                )
                .to_string()
            });
            let env = crate::prompt::PromptEnv::frozen(cwd, &date, frozen_runtime_env.as_ref());
            template.render(&env, agent_catalog.as_ref())
        },
    )
}

/// workflow agent 链能力事实（H2）：与调用点实际注入的 broker / permission_mode
/// 及冻结 disabled 集合同源（`build_workflow_middlewares` 的条件镜像，parity
/// 测试锁定）。生产调用点的 broker/permission_mode 恒 None ⇒ 审批通道无效
/// （`PermissionMiddleware::disabled()`），workflow prompt 因此不声明 10_hitl。
pub(crate) fn workflow_capabilities(
    disabled: &std::collections::HashSet<String>,
    broker_present: bool,
    permission_mode_present: bool,
) -> peri_agent::middleware::SectionCapabilities {
    crate::session::workflow_chain_capabilities(disabled, broker_present, permission_mode_present)
}

/// 按 workflow 能力事实投影冻结输入后的 system prompt（H2 生产路径共用）。
///
/// 生产不直接复制主会话冻结 prompt（其中包含 workflow 链不具备的审批 / 提问 /
/// 子代理声明），而是用同一份冻结输入（日期 / 语言 / 运行环境 / MetaHarness /
/// 基础段）按 workflow 能力投影重建；运行环境消费冻结快照（H3）。
pub(crate) fn project_workflow_system_prompt(
    frozen_data: &FrozenSessionData,
    capabilities: &peri_agent::middleware::SectionCapabilities,
    agent_catalog: &dyn AgentCatalogPort,
    cwd: &str,
) -> String {
    let collected = crate::session::build_collected_sections_with_capabilities(
        frozen_data.meta_harness(),
        None,
        frozen_data.language(),
        capabilities,
    );
    let template = crate::prompt::PromptTemplate::new(frozen_data.meta_harness(), &collected);
    let env = crate::prompt::PromptEnv::frozen(cwd, frozen_data.date(), frozen_data.runtime_env());
    template.render(&env, agent_catalog)
}

/// 创建 session 级 WorkflowMiddleware（session/new / load / resume 共用，GAP-05）。
///
/// 编排：构造 executor（`WorkflowAgentContext` 注入面）+ progress 通道 +
/// 经 [`WorkflowMiddlewareFactory`] 端口装配 `WorkflowMiddleware` 实例；
/// 返回端口句柄，host/stdio 命令面与 host/requests 命令面只持
/// `Arc<dyn WorkflowMiddlewarePort>`（3.0 批 2 波 2 装配边界收口）。
///
/// 事件发布：session 级路径与迁移前一致（`controller: None`），不启用事件
/// 发布——publish_hook 传 None，workflow 事件仅由内部 handler 消费
/// （usage/progress），不进入协议化事件流。TUI/stdio 主会话 session/new 均
/// 走此路径；每-turn executor 调用点（`host/prompt.rs` /
/// `host/stdio/session/prompt_exec.rs`）仍传 Some（与迁移前一致）。统一发射
/// 接线留待单独裁定。
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_session_workflow_middleware(
    provider: Arc<RwLock<LlmProvider>>,
    peri_config: &RwLock<PeriConfig>,
    cwd: &str,
    session_id: &str,
    frozen_data: &FrozenSessionData,
    middleware_factory: Arc<dyn WorkflowMiddlewareFactory>,
    publish_hook: Option<WorkflowPublishHook>,
    agent_catalog: Arc<dyn AgentCatalogPort>,
    mcp_skill_registry: Option<Arc<McpSkillRegistry>>,
    session_resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
) -> Option<Arc<dyn WorkflowMiddlewarePort>> {
    let compact_config = super::compact_config::load_compact_config(&peri_config.read());
    let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
    // H2：workflow agent 链的能力事实——与下面传入的 broker / permission_mode
    // 及冻结 disabled 集合同源（`build_workflow_middlewares` 的条件镜像，
    // parity 测试锁定）。本构造点的 broker/permission_mode 恒 None ⇒ 审批
    // 通道无效（`PermissionMiddleware::disabled()`），因此 workflow prompt 不
    // 声明 10_hitl；不再依赖「按执行类型过滤某段」的第二份名单。
    let workflow_broker: Option<Arc<dyn peri_acp_types::interaction::UserInteractionBroker>> = None;
    let workflow_permission_mode: Option<Arc<peri_acp_types::permission::SharedPermissionMode>> =
        None;
    let workflow_capabilities = workflow_capabilities(
        &frozen_data.meta_harness().disabled_middlewares,
        workflow_broker.is_some(),
        workflow_permission_mode.is_some(),
    );
    // H2：生产传入的 system prompt 必须是**按 workflow 能力投影**后的冻结输入
    // 重建（不是主会话冻结字节的复制——那会把 workflow 链不具备的审批/提问/
    // 子代理声明带给 workflow 模型）。
    let workflow_system_prompt = project_workflow_system_prompt(
        frozen_data,
        &workflow_capabilities,
        agent_catalog.as_ref(),
        cwd,
    );
    let wf_executor = create_executor(WorkflowAgentContext {
        cwd: cwd.to_string(),
        frozen_claude_md: frozen_data.claude_md().map(|s| s.to_string()),
        frozen_claude_local_md: frozen_data.claude_local_md().map(|s| s.to_string()),
        frozen_skill_summary: frozen_data.skill_summary().map(|s| s.to_string()),
        // W4b（F4/J5）：workflow agent 的技能来源 = 会话级 MCP registry（与主链
        // 同一份）；None = 未装配技能面（如 print/无会话 registry）。
        mcp_skill_registry,
        session_id: Some(session_id.to_string()),
        session_resources: Some(session_resources),
        compact_config: Some(compact_config),
        cancel: None,
        // H2：按 workflow 能力投影重建的冻结 prompt（不再复制主冻结 prompt）。
        system_prompt: Some(workflow_system_prompt),
        broker: workflow_broker,
        permission_mode: workflow_permission_mode,
        frozen_date: Some(frozen_data.date().to_string()),
        frozen_language: frozen_data.language().map(|s| s.to_string()),
        progress_tx: Some(progress_tx),
        subagent_ctx_builder: None,
        agent_prompt_builder: build_workflow_agent_prompt_builder(
            Arc::clone(&agent_catalog),
            frozen_data.meta_harness().clone(),
            workflow_capabilities,
            frozen_data.runtime_env().cloned(),
        ),
        model_factory: build_model_factory(&provider, peri_config),
        middleware_factory: Arc::clone(&middleware_factory),
        system_prompt_fallback: build_workflow_system_prompt_fallback(
            agent_catalog,
            frozen_data.meta_harness().clone(),
            workflow_capabilities,
            frozen_data.runtime_env().cloned(),
        ),
        forwarder_launcher: build_workflow_forwarder_launcher(),
        publish_hook,
        // Langfuse 观测：与迁移前一致（调用点均传 None，workflow agent 路径
        // 未启用遥测；注入面预留，未来接线经 LangfuseHooks 构造）。
        langfuse_hooks: None,
        langfuse_event_handler: None,
        // MetaHarness：装配期关闭集合（源自同一冻结数据，与段落覆盖同源——
        // 设计 §2.5，禁止重读配置）。
        meta_harness_disabled: frozen_data.meta_harness().disabled_middlewares.clone(),
    });
    let (notification_tx, _) = tokio::sync::broadcast::channel(32);
    Some(middleware_factory.build_workflow_middleware(
        wf_executor,
        cwd,
        notification_tx,
        Some(progress_rx),
    ))
}

// 类型锚点：确认端口装配方法的入参类型与编排处一致（防签名漂移）。
#[allow(dead_code)]
fn _type_anchor(
    f: Arc<dyn WorkflowMiddlewareFactory>,
    e: Arc<dyn AgentExecutor>,
    n: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
    p: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
) -> Arc<dyn WorkflowMiddlewarePort> {
    f.build_workflow_middleware(e, "cwd", n, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_model_factory_applies_concrete_model_name() {
        let provider = Arc::new(RwLock::new(LlmProvider::OpenAi {
            api_key: String::new(),
            base_url: "http://localhost".into(),
            model: "parent-model".into(),
            effort: None,
            max_tokens: 1024,
            context_1m: false,
            retry_observer: None,
        }));
        let config = RwLock::new(PeriConfig::default());
        let factory = build_model_factory(&provider, &config);
        let built = factory(Some("workflow-model"), None, Arc::new(|_| {}));

        assert_eq!(built.model_name, "workflow-model");
        assert_eq!(built.tier, None);
    }
}
