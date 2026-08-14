//! SessionStore — 会话生命周期数据 + FrozenContext
//!
//! SessionStore 构建后核心字段不可变（session_id、cwd、frozen），
//! 仅 `is_git_repo` 可在构建后设置。FrozenContext 保存会话开始即冻结的
//! 不可变数据（System Prompt、CLAUDE.md、Skills 摘要、日期等），全生命周期
//! 不可变，确保 Prompt Cache 前缀稳定性。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::thread::ThreadId;

// ─── SessionId ───────────────────────────────────────────────────────────────

/// Session 唯一标识符 — UUID v7（时间有序）
///
/// 会话创建时生成，全生命周期不变。UUID v7 保证时间有序，便于按创建时间排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(uuid::Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── FrozenContext ───────────────────────────────────────────────────────────

/// 会话级不可变上下文
///
/// 包含会话开始时一次性捕获的只读数据。所有字段使用 `Arc<str>` 共享，
/// 避免跨 Agent/Turn 复制大字符串。通过 [`FrozenContextBuilder`] 构建。
///
/// **冻结语义**：会话创建后不可修改。任何动态数据（如 Compact 续接、
/// goal steering）必须通过 `BaseMessage::human(...)` 注入，禁止触碰
/// System Prompt。
#[derive(Debug, Clone)]
pub struct FrozenContext {
    /// 完整 System Prompt（静态 + 动态占位符填充后）
    pub system_prompt: Arc<str>,
    /// CLAUDE.md 内容（项目级 + 用户级合并后）
    pub claude_md: Arc<str>,
    /// Skills 摘要（builtin + agm 加载的汇总）
    pub skill_summary: Arc<str>,
    /// 会话创建日期（用于 System Prompt 中的日期占位符）
    pub date: Arc<str>,
    /// 语言偏好（None 表示自动检测）
    pub language: Option<Arc<str>>,
    /// MetaHarness 冻结状态（段落覆盖 + middleware 关闭集合；会话内不可变，
    /// SubAgent/fork 复用——见 `docs/design/meta-harness-design.md` §2.3）。
    pub meta_harness: peri_acp_types::meta_harness::MetaHarnessState,
}

impl FrozenContext {
    /// 创建 Builder
    pub fn builder() -> FrozenContextBuilder {
        FrozenContextBuilder::default()
    }
}

// ─── FrozenContextBuilder ────────────────────────────────────────────────────

/// [`FrozenContext`] 的构建器
///
/// 每个字段可单独设置，未设置的字段使用空字符串或 None 作为默认值。
/// `language` 的 `Option` 语义：`None` = 不设置（保持 None），`Some(None)` = 显式清除，
/// `Some(Some(s))` = 设置具体值。
#[derive(Default)]
pub struct FrozenContextBuilder {
    system_prompt: Option<String>,
    claude_md: Option<String>,
    skill_summary: Option<String>,
    date: Option<String>,
    language: Option<Option<String>>,
    meta_harness: Option<peri_acp_types::meta_harness::MetaHarnessState>,
}

impl FrozenContextBuilder {
    pub fn system_prompt(mut self, s: impl Into<String>) -> Self {
        self.system_prompt = Some(s.into());
        self
    }

    pub fn claude_md(mut self, s: impl Into<String>) -> Self {
        self.claude_md = Some(s.into());
        self
    }

    pub fn skill_summary(mut self, s: impl Into<String>) -> Self {
        self.skill_summary = Some(s.into());
        self
    }

    pub fn date(mut self, s: impl Into<String>) -> Self {
        self.date = Some(s.into());
        self
    }

    pub fn language(mut self, s: Option<impl Into<String>>) -> Self {
        self.language = Some(s.map(Into::into));
        self
    }

    /// 设置 MetaHarness 冻结状态；未设置时 `build()` 使用默认空状态。
    pub fn meta_harness(mut self, state: peri_acp_types::meta_harness::MetaHarnessState) -> Self {
        self.meta_harness = Some(state);
        self
    }

    pub fn build(self) -> FrozenContext {
        FrozenContext {
            system_prompt: self.system_prompt.unwrap_or_default().into(),
            claude_md: self.claude_md.unwrap_or_default().into(),
            skill_summary: self.skill_summary.unwrap_or_default().into(),
            date: self.date.unwrap_or_default().into(),
            language: self.language.flatten().map(Into::into),
            meta_harness: self.meta_harness.unwrap_or_default(),
        }
    }
}

// ─── SessionStore ────────────────────────────────────────────────────────────

/// 会话生命周期数据
///
/// 核心字段（session_id、cwd、frozen）构建后不可变，仅 `is_git_repo`
/// 可在构建后设置。SessionStore 随 Session 创建，Session 销毁时释放。
///
/// - `session_id`：UUID v7 唯一标识
/// - `cwd`：工作目录（只读）
/// - `frozen`：会话级不可变上下文
/// - `is_git_repo`：是否在 git 仓库中（构建后可设置）
/// - `thread_id`：关联的 Thread（可选）
pub struct SessionStore {
    /// 会话唯一 ID
    pub session_id: SessionId,
    /// 工作目录（只读引用）
    pub cwd: Arc<str>,
    /// 冻结上下文（System Prompt / CLAUDE.md / Skills 等）
    pub frozen: FrozenContext,
    /// 是否在 git 仓库中（AtomicBool，构建后可设置）
    is_git_repo: AtomicBool,
    /// 关联的 Thread ID（可选）
    pub thread_id: Option<ThreadId>,
}

impl SessionStore {
    /// 创建新 SessionStore
    ///
    /// `is_git_repo` 默认为 false，需后续调用 `set_is_git_repo` 设置。
    pub fn new(cwd: Arc<str>, frozen: FrozenContext, thread_id: Option<ThreadId>) -> Self {
        Self {
            session_id: SessionId::new(),
            cwd,
            frozen,
            is_git_repo: AtomicBool::new(false),
            thread_id,
        }
    }

    /// 是否在 git 仓库中
    pub fn is_git_repo(&self) -> bool {
        self.is_git_repo.load(Ordering::Relaxed)
    }

    /// 设置 git 仓库状态
    pub fn set_is_git_repo(&self, value: bool) {
        self.is_git_repo.store(value, Ordering::Relaxed);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "store_test.rs"]
mod tests;
