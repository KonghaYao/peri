//! Session lifecycle management.
//!
//! Manages ACP session creation, loading, resumption, and closure.
//! Each session owns a ThreadStore entry, an Agent instance, and associated state.

pub mod agent_pool;
pub mod agent_runtime;
pub mod command;
pub mod event_sink;
pub mod executor;
pub mod frozen;
pub mod goal_state;
pub mod state_builders;

use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use dashmap::DashMap;
use peri_agent::{
    messages::BaseMessage,
    thread::{ThreadId, ThreadMeta, ThreadStore},
};
use peri_middlewares::{
    agent_define::AgentOverrides,
    prelude::{PermissionMode, SharedPermissionMode},
};
use tokio_util::sync::CancellationToken;

use crate::{
    provider::{
        config::{PeriConfig, ThinkingConfig},
        LlmProvider,
    },
    session::agent_runtime::{AgentRuntime, CancelPolicy},
};

pub struct AcpSession {
    pub session_id: String,
    pub thread_id: ThreadId,
    pub cwd: String,
    pub cancel_token: CancellationToken,
    pub state_messages: Vec<BaseMessage>,
    pub created_at: chrono::DateTime<Utc>,
    /// 当前激活的 provider ID（对应 PeriConfig.config.providers 中的 id）
    pub provider_id: String,
    /// 当前激活的模型别名（"opus"/"sonnet"/"haiku"）
    pub model_alias: String,
    /// 每会话独立的权限模式
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 每会话独立的 thinking 配置
    pub thinking: Option<ThinkingConfig>,
    /// 运行时 agent 实例（根 agent + 子 agent）
    pub active_agents: HashMap<ThreadId, AgentRuntime>,
    /// Goal steering 状态（session 级，跨 prompt 共享）
    pub goal_state: crate::session::goal_state::GoalState,
}

struct SessionManagerInner {
    sessions: DashMap<String, AcpSession>,
    thread_store: Arc<dyn ThreadStore>,
    provider: LlmProvider,
    peri_config: Arc<PeriConfig>,
    permission_mode: Arc<SharedPermissionMode>,
    /// Global agent overrides from CLI --agent flag (applied to all sessions)
    pub agent_overrides: Option<AgentOverrides>,
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<SessionManagerInner>,
}

impl SessionManager {
    pub fn new(
        thread_store: Arc<dyn ThreadStore>,
        provider: LlmProvider,
        peri_config: Arc<PeriConfig>,
        permission_mode: Arc<SharedPermissionMode>,
        agent_overrides: Option<AgentOverrides>,
    ) -> Self {
        Self {
            inner: Arc::new(SessionManagerInner {
                sessions: DashMap::new(),
                thread_store,
                provider,
                peri_config,
                permission_mode,
                agent_overrides,
            }),
        }
    }

    /// 使用指定 session_id 创建会话（用于 session/load 和 session/resume）
    pub async fn new_session_with_id(&self, session_id: &str, cwd: &str) -> anyhow::Result<()> {
        if self.inner.sessions.contains_key(session_id) {
            return Ok(());
        }

        let thread_id = ThreadId::from(session_id.to_string());
        let session = self.build_session(session_id, thread_id, cwd);

        self.inner.sessions.insert(session_id.to_string(), session);
        Ok(())
    }

    pub async fn new_session(&self, cwd: &str) -> anyhow::Result<(String, ThreadId)> {
        let meta = ThreadMeta::new(cwd);
        let thread_id = self.inner.thread_store.create_thread(meta).await?;

        let session_id = thread_id.clone();

        let session = self.build_session(&session_id, thread_id.clone(), cwd);

        self.inner.sessions.insert(session_id.clone(), session);
        Ok((session_id, thread_id))
    }

    /// 创建新会话并继承指定的 provider_id、model_alias 和 thinking 设置
    pub async fn new_session_with_settings(
        &self,
        cwd: &str,
        provider_id: String,
        model_alias: String,
        thinking: Option<ThinkingConfig>,
    ) -> anyhow::Result<(String, ThreadId)> {
        let meta = ThreadMeta::new(cwd);
        let thread_id = self.inner.thread_store.create_thread(meta).await?;

        let session_id = thread_id.clone();

        let session = AcpSession {
            session_id: session_id.clone(),
            thread_id: thread_id.clone(),
            cwd: cwd.to_string(),
            cancel_token: CancellationToken::new(),
            state_messages: Vec::new(),
            created_at: Utc::now(),
            provider_id,
            model_alias,
            permission_mode: SharedPermissionMode::new(PermissionMode::AutoMode),
            thinking,
            active_agents: HashMap::new(),
            goal_state: crate::session::goal_state::GoalState::new(
                Arc::new(peri_agent::goal::InMemoryGoalStore::new()),
                session_id.clone(),
            ),
        };

        self.inner.sessions.insert(session_id.clone(), session);
        Ok((session_id, thread_id))
    }

    fn build_session(&self, session_id: &str, thread_id: ThreadId, cwd: &str) -> AcpSession {
        AcpSession {
            session_id: session_id.to_string(),
            thread_id,
            cwd: cwd.to_string(),
            cancel_token: CancellationToken::new(),
            state_messages: Vec::new(),
            created_at: Utc::now(),
            provider_id: self.inner.peri_config.config.active_provider_id.clone(),
            model_alias: self.inner.peri_config.config.active_alias.clone(),
            permission_mode: SharedPermissionMode::new(PermissionMode::AutoMode),
            thinking: self.inner.peri_config.config.thinking.clone(),
            active_agents: HashMap::new(),
            goal_state: crate::session::goal_state::GoalState::new(
                Arc::new(peri_agent::goal::InMemoryGoalStore::new()),
                session_id.to_string(),
            ),
        }
    }

    pub async fn close_session(&self, session_id: &str) -> anyhow::Result<()> {
        if let Some((_, session)) = self.inner.sessions.remove(session_id) {
            // 取消所有运行时 agent 实例
            for runtime in session.active_agents.values() {
                runtime.cancel_token.cancel();
            }
            session.cancel_token.cancel();
        }
        Ok(())
    }

    pub async fn list_sessions(&self) -> anyhow::Result<Vec<ThreadMeta>> {
        self.inner.thread_store.list_threads().await
    }

    pub fn get_session(
        &self,
        session_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, AcpSession>> {
        self.inner.sessions.get(session_id)
    }

    pub fn get_session_mut(
        &self,
        session_id: &str,
    ) -> Option<dashmap::mapref::one::RefMut<'_, String, AcpSession>> {
        self.inner.sessions.get_mut(session_id)
    }

    pub fn inner_sessions(&self) -> &DashMap<String, AcpSession> {
        &self.inner.sessions
    }

    pub fn cancel_session(&self, session_id: &str) {
        if let Some(mut session) = self.inner.sessions.get_mut(session_id) {
            // Cancel all cascade-policy agents first
            for runtime in session.active_agents.values() {
                if runtime.cancel_policy == CancelPolicy::Cascade {
                    runtime.cancel_token.cancel();
                }
            }

            // Cancel the current token so all clones (held by link tasks,
            // permission loops) detect cancellation. Then replace with a fresh
            // token so subsequent prompts on the same session are not affected.
            // CancellationToken has no reset() — once cancelled it stays cancelled.
            session.cancel_token.cancel();
            session.cancel_token = CancellationToken::new();
        }
    }

    pub fn provider(&self) -> &LlmProvider {
        &self.inner.provider
    }

    pub fn peri_config(&self) -> &Arc<PeriConfig> {
        &self.inner.peri_config
    }

    pub fn permission_mode(&self) -> &Arc<SharedPermissionMode> {
        &self.inner.permission_mode
    }

    pub fn thread_store(&self) -> &Arc<dyn ThreadStore> {
        &self.inner.thread_store
    }

    pub fn agent_overrides(&self) -> Option<&AgentOverrides> {
        self.inner.agent_overrides.as_ref()
    }

    /// 构建会话级 frozen 数据（统一构造入口，消除 TUI/stdio 重复 5 处）。
    ///
    /// 封装 [`crate::session::frozen::build_frozen_session_data`]，
    /// 使用 manager 内部捕获的 `peri_config.language` 和当前日期。
    pub fn build_frozen_data(
        &self,
        cwd: &str,
        plugin_skill_dirs: &[std::path::PathBuf],
        plugin_agent_dirs: &[std::path::PathBuf],
    ) -> crate::session::executor::FrozenSessionData {
        let frozen_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let frozen_language = self.inner.peri_config.config.language.clone();
        crate::session::frozen::build_frozen_session_data(
            cwd,
            frozen_language.as_deref(),
            plugin_skill_dirs,
            plugin_agent_dirs,
            &frozen_date,
        )
    }

    /// 确保指定 session 在 SessionManager 中存在 AcpSession 记录，
    /// 用于支撑 cascade cancel 子 agent 与 goal_state 跨 prompt 共享。
    ///
    /// 如果 session 已存在则 no-op；否则插入一个空 history 的 AcpSession。
    /// TUI/stdio 调用方仍自行维护 history/frozen/agent_pool 等字段，
    /// SessionManager 只负责 active_agents / goal_state 维度。
    pub fn ensure_session(&self, session_id: &str, cwd: &str) {
        if self.inner.sessions.contains_key(session_id) {
            return;
        }
        let thread_id = ThreadId::from(session_id.to_string());
        let session = self.build_session(session_id, thread_id, cwd);
        self.inner.sessions.insert(session_id.to_string(), session);
    }

    /// 取指定 session 的 goal_state 句柄（用于 TUI/stdio 注入到 middleware 链）。
    ///
    /// 调用方应先调用 [`ensure_session`] 保证记录存在。
    /// 不存在时返回 None。
    pub fn goal_state_for(
        &self,
        session_id: &str,
    ) -> Option<crate::session::goal_state::GoalState> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.goal_state.clone())
    }

    /// 取消指定 session 的所有 cascade 子 agent（暴露给 TUI/stdio 用于 session/cancel）。
    pub fn cancel_cascade_children_for(&self, session_id: &str) {
        if let Some(session) = self.inner.sessions.get(session_id) {
            session.cancel_cascade_children();
        }
    }
}

impl AcpSession {
    /// 取消指定 agent 的所有 cascade 子 agent
    pub fn cancel_cascade_children(&self) {
        for runtime in self.active_agents.values() {
            if runtime.cancel_policy == CancelPolicy::Cascade {
                runtime.cancel_token.cancel();
            }
        }
    }

    /// 取消所有 agent（session 结束时）
    pub fn cancel_all_agents(&self) {
        for runtime in self.active_agents.values() {
            runtime.cancel_token.cancel();
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
