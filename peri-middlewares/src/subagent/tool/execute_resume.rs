//! SubAgent 恢复路径（`resume_thread_id`）：v2 stages 实现
//!
//! 语义（issue 决策）：主 agent 凭中断/错误/bg 通知文本携带的 `child_thread_id`
//! 恢复被中断 subagent——从磁盘 thread 恢复现场继续执行，不创建新 subagent。
//! - 工具集：`meta.title == "fork"` → 父工具集 clone（execute_fork.rs 同款，无过滤）；
//!   否则 `load_agent_def(title)` 重新应用 tools/disallowed 过滤（权限漂移防护，
//!   issue 决策 11）
//! - skill_names 恒不注入（R-H1：旧 transcript 已含首轮 SkillPreload 内容，
//!   重复注入会随多次恢复无界增长；`SubagentResumeConfig` 无该字段，结构性禁止）
//! - 不传 system_prompt（F4：identity System 已在旧 transcript 中，重复注入会重复）
//! - run mode 由本次调用决定（issue 决策 8）：`run_in_background: true` →
//!   Background（新 task_id + TaskManager 注册）
//!
//! L3：校验/重建/运行/收尾统一经
//! `peri_agent::session::subagent::resume_subagent`（Agent 层统一入口），
//! 本文件只组装意图（[`SubagentResumeConfig`]）。

use std::sync::Arc;

use peri_agent::agent::react::ReactLLM;
use peri_agent::session::subagent::{
    extract_last_ai_text, format_subagent_result, SessionFactory, SubagentCancelPolicy,
    SubagentResumeConfig, SubagentRunMode, SubagentSpawned,
};
use peri_agent::thread::ThreadStore;
use peri_agent::tools::BaseTool;

use crate::tool_search::ExecuteExtraToolResolver;

impl super::SubAgentTool {
    /// 恢复被中断 subagent（唯一调用方：define.rs invoke 的 resume 分支）。
    ///
    /// 前置校验（define.rs 已做）：resume_thread_id 与 fork / subagent_type 互斥、
    /// thread_store 存在。本方法 load_meta 取 title 决定工具集 / 迭代上限，
    /// 组装 [`SubagentResumeConfig`] 经 [`SessionFactory::resume_subagent`] 执行。
    ///
    /// 返回文本与 spawn 路径一致：
    /// - Background → 启动确认（task_id 文本，execute_bg.rs 同款；thread 恒存在 → 带 thread）
    /// - interrupted → slice 1 格式（`child_thread_id: {id}` + 恢复提示）
    /// - 完成 → `child_thread_id: {id}\n{result}`（thread_store 恒存在）
    /// - 错误 → 原样 Err（agent 层已带 `resume_subagent:` 前缀）
    pub(crate) async fn invoke_resume(
        &self,
        thread_id: String,
        prompt: Option<String>,
        cwd: String,
        run_in_background: bool,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let host = self.host();
        // 双保险：define.rs 已拦截 thread_store 缺失（恢复需要持久化现场）
        let thread_store = host.as_ref().and_then(|h| h.thread_store.clone()).ok_or(
            "resume_subagent: thread store required (resume_thread_id needs a persisted thread)",
        )?;

        // 双保险（review MEDIUM-1）：bg resume 在 agent 层注册失败会回滚 status，
        // 但无 task_manager 时先在此预检，避免「置 active → 注册失败 → 回滚」的
        // 无效往返（execute_bg.rs:29-32 同款文本）
        if run_in_background && host.as_ref().and_then(|h| h.task_manager.clone()).is_none() {
            return Err("Background tasks not available: no task manager configured".into());
        }

        // 0. thread_id 格式校验（review low-2）：FilesystemThreadStore 按 id 拼路径，
        //    非 UUID 在 load_meta 之前统一拒绝（agent 层同文本，双保险）
        if uuid::Uuid::parse_str(&thread_id).is_err() {
            return Err(format!("resume_subagent: invalid thread id: {}", thread_id).into());
        }

        // 1. load_meta 取 title（决定工具集恢复路径，issue 决策 11）
        let meta = thread_store
            .load_meta(&thread_id)
            .await
            .map_err(|_| format!("resume_subagent: thread not found: {}", thread_id))?;
        let title = meta.title.clone().unwrap_or_default();

        // 2. 按 title 恢复工具集 / LLM / 迭代上限：
        //    - "fork" → 父工具集 clone（execute_fork.rs 同款，无过滤）+ 200 迭代
        //    - 其他 → load_agent_def(title) 重新应用过滤（tools/disallowed，
        //      权限漂移防护）+ agent_def 声明的 max_turns
        //    二者均不注入 skill_names / system_prompt（R-H1 / F4）
        let (llm, tools, max_iterations) = if title == "fork" {
            let llm = (self.llm_factory)(None);
            let tools: Vec<Arc<dyn BaseTool>> = self.parent_tools.iter().cloned().collect();
            (llm, tools, 200)
        } else {
            let agent_def = self
                .load_agent_def(&title, &cwd)
                .map_err(|e| format!("resume_subagent: {}", e))?;
            let build_result = self
                .build_agent_from_def(
                    &agent_def,
                    &title,
                    &cwd,
                    SubagentCancelPolicy::Cascade,
                    false,
                    true,
                )
                .await?;
            let llm = build_result.llm;
            let tools: Vec<Arc<dyn BaseTool>> = build_result
                .tools
                .into_iter()
                .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
                .collect();
            (llm, tools, build_result.max_iterations)
        };

        // 3. 组装 resume config（通道段与 spawn_config_base 同源；五字段 None 与
        //    spawn 一致；tool_invocation_resolver 显式设置，R2 补充）
        let config = self.resume_config_base(
            thread_id.clone(),
            prompt,
            if run_in_background {
                SubagentRunMode::Background
            } else {
                SubagentRunMode::Sync
            },
            max_iterations,
            llm,
            tools,
            thread_store,
            cwd,
        );

        // 4. 统一恢复入口（Agent 层完成校验 / 重建 / 执行 / 收尾）
        let spawned = self.resume(config).await?;

        // 5. 返回文本（见方法注释格式约定）
        if let Some(task_id) = &spawned.task_id {
            return Ok(format!(
                "Background task {} started (thread: {}). You will be notified when it completes. \
                 You can continue with other tasks in the meantime.",
                task_id, spawned.child_thread_id
            ));
        }

        if spawned.interrupted {
            return Ok(format!(
                "child_thread_id: {}\nSub-agent execution was interrupted, resume with Agent(resume_thread_id: {})",
                spawned.child_thread_id, spawned.child_thread_id
            ));
        }

        Ok(format!(
            "child_thread_id: {}\n{}",
            spawned.child_thread_id,
            format_subagent_result(&peri_agent::agent::react::AgentOutput {
                text: extract_last_ai_text(&spawned.session),
                steps: 0,
                tool_calls: Vec::new(),
                stop_reason: None,
                block_continue: None,
            })
        ))
    }

    /// 组装 [`SubagentResumeConfig`] 公共部分（通道段逐字段对照
    /// [`spawn_config_base`]：error_suggest_registry / tool_registry_snapshot /
    /// compact_config / context_budget / compact_llm 恒 None 与 spawn 一致；
    /// `tool_invocation_resolver: Some(ExecuteExtraToolResolver::default())`
    /// 显式设置保持包装层语义，R2 补充）。
    ///
    /// `agent_name` 恒传 None——由 agent 层从 `meta.title` 取（R2 补充：避免
    /// 双源；thread 创建时 title 已固化 = spawn 时的 agent_name）。
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    pub(crate) fn resume_config_base(
        &self,
        thread_id: String,
        prompt: Option<String>,
        run_mode: SubagentRunMode,
        max_iterations: usize,
        llm: Box<dyn ReactLLM + Send + Sync>,
        tools: Vec<Arc<dyn BaseTool>>,
        thread_store: Arc<dyn ThreadStore>,
        cwd: String,
    ) -> SubagentResumeConfig {
        let host = self.host();
        let (on_subagent_start, on_subagent_stop) = self.lifecycle_closures();
        SubagentResumeConfig {
            thread_id,
            prompt,
            agent_name: None,
            run_mode,
            max_iterations,
            llm,
            chain_assembler: Arc::clone(&self.chain_assembler),
            tools,
            tool_invocation_resolver: Some(Arc::new(ExecuteExtraToolResolver::default())),
            error_suggest_registry: None,
            tool_registry_snapshot: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            thread_store,
            event_handler: self.event_handler.clone(),
            bg_event_sender: host.as_ref().and_then(|h| h.bg_event_sender.clone()),
            task_manager: host.as_ref().and_then(|h| h.task_manager.clone()),
            on_bg_complete: host.as_ref().and_then(|h| h.on_bg_complete.clone()),
            langfuse_bridge: host.as_ref().and_then(|h| h.langfuse_bridge.clone()),
            on_subagent_start,
            on_subagent_stop,
            register_runtime: host.as_ref().and_then(|h| h.register_runtime.clone()),
            deregister_runtime: host.as_ref().and_then(|h| h.deregister_runtime.clone()),
            parent_agent_id: *self.parent_agent_id.read(),
            // 父侧数据回退（parent session 存在时由 resume_subagent 覆盖）
            cancel_token: self.cancel.clone(),
            cwd: Some(cwd),
            frozen_claude_md: host
                .as_ref()
                .and_then(|h| h.frozen_claude_md.as_deref().map(|s| s.to_string())),
            frozen_claude_local_md: host
                .as_ref()
                .and_then(|h| h.frozen_claude_local_md.as_deref().map(|s| s.to_string())),
            frozen_skill_summary: host
                .as_ref()
                .and_then(|h| h.frozen_skill_summary.as_deref().map(|s| s.to_string())),
            frozen_date: None,
        }
    }

    /// 调用统一恢复入口（parent 存在时 frozen copy / parent 链校验自 parent
    /// session 读取；与 [`spawn`] :402-408 同款包装）。
    pub(crate) async fn resume(
        &self,
        config: SubagentResumeConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        let parent = self.parent_session.read().clone();
        SessionFactory::resume_subagent(parent.as_ref(), config).await
    }
}
