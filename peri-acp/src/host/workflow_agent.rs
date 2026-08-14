//! Workflow agent 装配面薄壳（p1-wa 收口）。
//!
//! 执行体已随 p1-wa 物理迁入 `peri_agent::agent::workflow`（`agent.rs` /
//! `factory.rs`——session 运行单元归 Agent 层，§2）；中间件链 / 工具 /
//! error_suggest / tool resolver / session 级 WorkflowMiddleware 装配经
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
    compact::CompactConfig,
    ports::{SkillsPort, WorkflowMiddlewarePort},
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
        crate::event::spawn_eventbus_forwarder(handles, on_event, None);
    })
}

/// system prompt fallback 渲染闭包构造（`PromptTemplate` 渲染面；skills 经
/// 注入的 [`SkillsPort`] 访问——与宿主装配点注入的端口实现同一类型）。
///
/// 16_workflow 已删除（C2），workflow agent 渲染与主链共用同一段落来源；
/// `meta_harness` 为冻结期 MetaHarnessState（随调用点从 `FrozenSessionData`
/// 注入，段落覆盖与主会话同源——禁止重读配置，设计 §2.4）。
pub(crate) fn build_workflow_system_prompt_fallback(
    skills: Arc<dyn SkillsPort>,
    meta_harness: peri_acp_types::meta_harness::MetaHarnessState,
) -> WorkflowSystemPromptFallback {
    Arc::new(
        move |cwd: &str, frozen_date: Option<&str>, frozen_language: Option<&str>| {
            // C3：detect 无参（gate 判定随段落实体迁移至持有者装配判定；
            // workflow 渲染与主链共用同一段落来源——C2 决定）
            let features = crate::prompt::PromptFeatures::detect();
            // C2：收集结果 = 渲染面静态声明（冻结 disabled 集合 + 冻结语言
            // 驱动；fallback 无 overrides）。
            // advisor 裁决 B（2026-08-14）：workflow agent 链不装配
            // HumanInTheLoopMiddleware（broker: None），10_hitl 描述的是主
            // 会话审批机制——对 workflow 模型是误导性指令；presence-is-the-gate
            // 契约要求在无 HITL 的渲染路径排除该段（C3 D5 决策修订）。
            let collected =
                crate::session::build_collected_sections(&meta_harness, None, frozen_language)
                    .into_iter()
                    .filter(|s| s.id != "10_hitl")
                    .collect::<Vec<_>>();
            let template = crate::prompt::PromptTemplate::new(&meta_harness, &collected);
            let env = if let Some(date) = frozen_date {
                crate::prompt::PromptEnv::with_frozen_date(cwd, date)
            } else {
                crate::prompt::PromptEnv::detect(cwd)
            };
            template.render(&env, &features, skills.as_ref(), &[])
        },
    )
}

/// workflow `agentType` 指定时的 subagent prompt 渲染器。
///
/// 与主链注入的 `system_builder` 使用相同的 PromptTemplate 语义；
/// 16_workflow 已删除（C2），无子面向 feature 差异。
///
/// `meta_harness` 为冻结期 MetaHarnessState（同源注入，见
/// `build_workflow_system_prompt_fallback`）。
pub(crate) fn build_workflow_agent_prompt_builder(
    skills: Arc<dyn SkillsPort>,
    meta_harness: peri_acp_types::meta_harness::MetaHarnessState,
) -> WorkflowAgentPromptBuilder {
    Arc::new(
        move |overrides: Option<&AgentOverrides>, cwd, frozen_date, frozen_language| {
            // C3：detect 无参（同 build_workflow_system_prompt_fallback）
            let features = crate::prompt::PromptFeatures::detect();
            // C2：收集结果 = 渲染面静态声明（冻结 disabled 集合 + overrides +
            // 冻结语言驱动；persona 段内容依赖 overrides，调用期计算）。
            // advisor 裁决 B：workflow 链无 HumanInTheLoopMiddleware，
            // 排除 10_hitl（同 build_workflow_system_prompt_fallback）。
            let collected =
                crate::session::build_collected_sections(&meta_harness, overrides, frozen_language)
                    .into_iter()
                    .filter(|s| s.id != "10_hitl")
                    .collect::<Vec<_>>();
            let template = crate::prompt::PromptTemplate::new(&meta_harness, &collected);
            let env = frozen_date.map_or_else(
                || crate::prompt::PromptEnv::detect(cwd),
                |date| crate::prompt::PromptEnv::with_frozen_date(cwd, date),
            );
            template.render(&env, &features, skills.as_ref(), &[])
        },
    )
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
    skills: Arc<dyn SkillsPort>,
) -> Option<Arc<dyn WorkflowMiddlewarePort>> {
    let mut compact_config = CompactConfig::default();
    compact_config.apply_env_overrides();
    let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
    let wf_executor = create_executor(WorkflowAgentContext {
        cwd: cwd.to_string(),
        frozen_claude_md: frozen_data.claude_md().map(|s| s.to_string()),
        frozen_claude_local_md: frozen_data.claude_local_md().map(|s| s.to_string()),
        frozen_skill_summary: frozen_data.skill_summary().map(|s| s.to_string()),
        session_id: Some(session_id.to_string()),
        compact_config: Some(compact_config),
        cancel: None,
        // 16_workflow 已删除（C2）：子面向 prompt 与主 prompt 字节相同，
        // 直接复用主冻结 prompt（subagent_system_prompt 字段已随 C5 移除）。
        system_prompt: Some(frozen_data.system_prompt().to_string()),
        broker: None,
        permission_mode: None,
        frozen_date: Some(frozen_data.date().to_string()),
        frozen_language: frozen_data.language().map(|s| s.to_string()),
        thread_store: None,
        progress_tx: Some(progress_tx),
        subagent_ctx_builder: None,
        agent_prompt_builder: build_workflow_agent_prompt_builder(
            Arc::clone(&skills),
            frozen_data.meta_harness().clone(),
        ),
        model_factory: build_model_factory(&provider, peri_config),
        middleware_factory: Arc::clone(&middleware_factory),
        system_prompt_fallback: build_workflow_system_prompt_fallback(
            skills,
            frozen_data.meta_harness().clone(),
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
