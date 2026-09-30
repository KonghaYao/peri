//! # peri-middlewares
//!
//! Rust middleware implementations aligned with `@langgraph-js/agent-middlewares` (TypeScript).
//!
//! ## 文件系统与终端
//!
//! 不再有 middleware 提供面（v4-part-4 W3-C1）：7 个文件/终端工具
//! （`Read` / `Write` / `Edit` / `Glob` / `Grep` / `folder_operations` / `Bash`）
//! 由 builtin `workspace` MCP 实例提供，模型面使用原始工具名。
//! 工具实现位于 `peri-mcp-workspace` 包的 `filesystem` 与 `terminal` 模块。

#![allow(
    clippy::type_complexity,
    clippy::empty_line_after_doc_comments,
    clippy::useless_conversion
)]
//! ## 认知增强与安全（原 rust-standard-middlewares）
//! - [`AgentsMdMiddleware`]：注入 AGENTS.md / CLAUDE.md 项目指引
//! - [`SkillsMiddleware`]：渐进式 Skills 摘要注入
//! - [`PermissionMiddleware`]：敏感工具调用前需用户确认
//! - [`HumanInTheLoopMiddleware`]：向用户提问的通道（AskUserQuestion 工具）

pub mod agents_md;
pub mod assembly;
mod completion_reminder;
pub mod goal;
/// 装配注入端口实现（3.0 批 2 波 2：`PluginManager` / `AgentCatalogProvider`）。
pub mod host_ports;
pub mod subagent;

pub mod ask_user;
pub mod attribution;
pub mod default_system_prompt;
pub mod hitl;
pub mod hooks;
pub mod lsp;
pub mod mcp;
pub mod middleware;
pub mod permission;
pub mod plugin;
pub mod ptc;
pub use plugin::{
    AvailablePlugin, ClaudeSettings, CommandEntry, CommandProvider, CommandSource, InstallScope,
    InstalledPlugin, InstalledPlugins, KnownMarketplace, LoadedPlugin, LoaderError,
    MarketplaceEntry, MarketplaceError, MarketplaceManager, MarketplaceManifest, MarketplacePlugin,
    MarketplaceRefreshEvent, MarketplaceSource, PluginAgent, PluginAuthor, PluginChannel,
    PluginCommand, PluginCommandEntry, PluginCommandProvider, PluginConfigError, PluginLspServer,
    PluginManifest, PluginMiddleware, PluginOption,
};
pub mod at_mention;
pub mod skills;
pub mod tool_search;
pub mod tools;
pub mod workflow;

/// v4 引名约定锁定：prompt 文本引用的 builtin 工具名必须与注册表一致（跨 crate 的
/// prompt 文本扫描，见 [`prompt_tool_name_lock_tests`] 的边界说明）。
#[cfg(test)]
#[path = "prompt_tool_name_lock_test.rs"]
mod prompt_tool_name_lock_tests;

pub use agents_md::AgentsMdMiddleware;
pub use ask_user::{
    ask_user_tool_definition, parse_ask_user, InteractionContext, QuestionItem, QuestionOption,
};
pub use at_mention::AtMentionMiddleware;
pub use attribution::GitAttributionMiddleware;
pub use default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
pub use goal::GoalMiddleware;
pub use hitl::HumanInTheLoopMiddleware;
pub use lsp::LspSyncMiddleware;
pub use middleware::image::ImageMiddleware;
pub use peri_acp_types::agents::AgentOverrides;
pub use permission::{
    default_requires_approval, effective_tool_name, AutoClassifier, BatchItem, Classification,
    HitlDecision, LlmAutoClassifier, PermissionMiddleware, PermissionMode, SharedPermissionMode,
};
pub mod settings;

pub use settings::{load_disable_bundled_skills, load_global_skills_dir};
pub use skills::{resolve_skill_roots, SkillMetadata, SkillRoot, SkillsMiddleware};
pub use subagent::{
    infer_agent_capability, AgentCapability, SkillPreloadMiddleware, SubAgentMiddleware,
    SubAgentTool,
};
pub use tool_search::{
    resolve_effective_tool_name, ExecuteExtraToolResolver, ToolSearchMiddleware,
    EXECUTE_EXTRA_TOOL_NAME, EXTRA_TOOL_NAME_FIELD, EXTRA_TOOL_PARAMS_FIELD,
    SEARCH_EXTRA_TOOLS_NAME,
};
pub use tools::{ArcToolWrapper, AskUserTool, BoxToolWrapper};

/// Prelude - 常用类型一次性导入
pub mod prelude {
    // 重导出 peri-agent 核心类型
    pub use peri_agent::prelude::*;

    pub use crate::{
        agents_md::AgentsMdMiddleware,
        ask_user::{
            ask_user_tool_definition, parse_ask_user, InteractionContext, QuestionItem,
            QuestionOption,
        },
        attribution::GitAttributionMiddleware,
        hitl::HumanInTheLoopMiddleware,
        hooks::{HookMiddleware, RegisteredHook},
        middleware::TodoMiddleware,
        permission::{
            default_requires_approval, AutoClassifier, BatchItem, Classification, HitlDecision,
            LlmAutoClassifier, PermissionMiddleware, PermissionMode, SharedPermissionMode,
        },
        plugin::{
            AvailablePlugin, ClaudeSettings, CommandEntry, CommandProvider, CommandSource,
            InstallScope, InstalledPlugin, InstalledPlugins, KnownMarketplace, LoadedPlugin,
            LoaderError, MarketplaceEntry, MarketplaceError, MarketplaceManager,
            MarketplaceManifest, MarketplacePlugin, MarketplaceRefreshEvent, MarketplaceSource,
            PluginAgent, PluginAuthor, PluginChannel, PluginCommand, PluginCommandProvider,
            PluginConfigError, PluginLspServer, PluginManifest, PluginMiddleware, PluginOption,
        },
        skills::{SkillMetadata, SkillsMiddleware},
        subagent::{SkillPreloadMiddleware, SubAgentMiddleware, SubAgentTool},
        tools::{ArcToolWrapper, AskUserTool, BoxToolWrapper, TodoItem, TodoStatus, TodoWriteTool},
    };
}
