//! # peri-middlewares
//!
//! Rust middleware implementations aligned with `@langgraph-js/agent-middlewares` (TypeScript).
//!
//! ## 文件系统与终端（原 peri-middlewares）

#![allow(
    clippy::type_complexity,
    clippy::empty_line_after_doc_comments,
    clippy::useless_conversion
)]
//! - [`middleware::FilesystemMiddleware`]：文件系统操作
//! - [`middleware::TerminalMiddleware`]：终端命令执行
//!
//! ## 认知增强与安全（原 rust-standard-middlewares）
//! - [`AgentsMdMiddleware`]：注入 AGENTS.md / CLAUDE.md 项目指引
//! - [`SkillsMiddleware`]：渐进式 Skills 摘要注入
//! - [`PermissionMiddleware`]：敏感工具调用前需用户确认
//! - [`HumanInTheLoopMiddleware`]：向用户提问的通道（AskUserQuestion 工具）

pub mod agent_define;
pub mod agents_md;
pub mod artifact;
pub mod assembly;
pub mod claude_agent_parser;
pub mod goal;
pub mod goal_middleware;
/// 装配注入端口实现（3.0 批 2 波 2：`PluginManager` / `SkillsProvider`）。
pub mod host_ports;
pub mod subagent;
pub use claude_agent_parser::{
    format_agent_id, parse_agent_file, ClaudeAgent, ClaudeAgentFrontmatter, ToolsValue,
};
pub mod ask_user;
pub mod attribution;
pub mod cron;
pub mod default_system_prompt;
pub mod error_suggest;
pub mod hitl;
pub mod hooks;
pub mod lsp;
pub mod mcp;
pub mod meta_harness;
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

pub use agent_define::{AgentDefineMiddleware, AgentOverrides};
pub use agents_md::AgentsMdMiddleware;
pub use ask_user::{
    ask_user_tool_definition, parse_ask_user, InteractionContext, QuestionItem, QuestionOption,
};
pub use at_mention::AtMentionMiddleware;
pub use attribution::GitAttributionMiddleware;
pub use cron::{CronMiddleware, CronScheduler, CronTask, CronTrigger};
pub use default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
pub use goal_middleware::GoalMiddleware;
pub use hitl::HumanInTheLoopMiddleware;
pub use lsp::{LspMiddleware, LspTool};
pub use middleware::image::ImageMiddleware;
pub use permission::{
    default_requires_approval, effective_tool_name, AutoClassifier, BatchItem, Classification,
    HitlDecision, LlmAutoClassifier, PermissionMiddleware, PermissionMode, SharedPermissionMode,
};
pub use skills::{
    list_skills, load_global_skills_dir, load_skill_metadata, SkillMetadata, SkillsMiddleware,
};
pub use subagent::{
    infer_agent_capability, scan_agents, scan_agents_detailed, scan_agents_with_extra_dirs,
    AgentCapability, SkillPreloadMiddleware, SubAgentMiddleware, SubAgentTool,
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
        agent_define::AgentDefineMiddleware,
        agents_md::AgentsMdMiddleware,
        ask_user::{
            ask_user_tool_definition, parse_ask_user, InteractionContext, QuestionItem,
            QuestionOption,
        },
        attribution::GitAttributionMiddleware,
        cron::{CronMiddleware, CronScheduler, CronTask, CronTrigger},
        hitl::HumanInTheLoopMiddleware,
        hooks::{HookMiddleware, RegisteredHook},
        middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
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
        tools::{
            ArcToolWrapper, AskUserTool, BoxToolWrapper, EditFileTool, FolderOperationsTool,
            GlobFilesTool, GrepTool, ReadFileTool, TodoItem, TodoStatus, TodoWriteTool,
            WriteFileTool,
        },
    };
}
