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
//! - [`HumanInTheLoopMiddleware`]：敏感工具调用前需用户确认

pub mod agent_define;
pub mod agents_md;
pub mod claude_agent_parser;
pub mod compact_middleware;
pub mod goal;
pub mod goal_middleware;
pub mod subagent;
pub use claude_agent_parser::{
    format_agent_id, parse_agent_file, ClaudeAgent, ClaudeAgentFrontmatter, ToolsValue,
};
pub mod ask_user;
pub mod attribution;
pub mod cron;
pub mod error_suggest;
pub mod hitl;
pub mod hooks;
pub mod lsp;
pub mod mcp;
pub mod middleware;
pub mod plugin;
pub mod process;
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

pub use agent_define::{AgentDefineMiddleware, AgentOverrides};
pub use agents_md::AgentsMdMiddleware;
pub use ask_user::{
    ask_user_tool_definition, parse_ask_user, AskUserBatchRequest, AskUserOption,
    AskUserQuestionData,
};
pub use at_mention::AtMentionMiddleware;
pub use attribution::GitAttributionMiddleware;
pub use cron::{CronMiddleware, CronScheduler, CronTask, CronTrigger};
pub use goal_middleware::GoalMiddleware;
pub use hitl::{
    default_requires_approval, effective_tool_name, is_yolo_mode, AutoClassifier, BatchItem,
    Classification, HitlDecision, HumanInTheLoopMiddleware, LlmAutoClassifier, PermissionMode,
    SharedPermissionMode,
};
pub use lsp::{LspMiddleware, LspTool};
pub use skills::{
    list_skills, load_global_skills_dir, load_skill_metadata, SkillMetadata, SkillsMiddleware,
};
pub use subagent::{
    infer_agent_capability, scan_agents, scan_agents_detailed, scan_agents_with_extra_dirs,
    AgentCapability, BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus,
    SkillPreloadMiddleware, SubAgentMiddleware, SubAgentTool,
};
pub use tool_search::{
    is_deferred_tool, resolve_effective_tool_name, ToolSearchMiddleware, CORE_TOOLS,
    EXECUTE_EXTRA_TOOL_NAME, EXTRA_TOOL_NAME_FIELD, EXTRA_TOOL_PARAMS_FIELD, META_TOOLS,
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
            ask_user_tool_definition, parse_ask_user, AskUserBatchRequest, AskUserOption,
            AskUserQuestionData,
        },
        attribution::GitAttributionMiddleware,
        cron::{CronMiddleware, CronScheduler, CronTask, CronTrigger},
        hitl::{
            default_requires_approval, is_yolo_mode, AutoClassifier, BatchItem, Classification,
            HitlDecision, HumanInTheLoopMiddleware, LlmAutoClassifier, PermissionMode,
            SharedPermissionMode,
        },
        hooks::{HookMiddleware, RegisteredHook},
        middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
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
