//! MCP skill 异步发现：SEP-2640（`skills/list` + digest 校验 + frontmatter
//! 比对）为规范路径；未声明 Skills 扩展的旧 server 走 legacy 兼容兜底
//! （`resources/list` 扫描 `skill://` 前缀资源）。
//!
//! 异步语义（spec B.1）：不阻塞连接初始化与首 turn；发现完成静默（不注入
//! agent 上下文）；失败仅告警日志，不影响连接。任务由 McpMiddleware 的
//! `before_agent` 投影 spawn，持 session cancel token。
//!
//! 规范要点（SEP-2640 v1，2026-08-05 定稿）：
//! - host MUST NOT 仅凭 URI scheme 断定资源是技能——发现走 `skills/list`
//!   （server 在 capabilities.extensions 声明 `io.modelcontextprotocol/skills`）；
//! - skill name = `SKILL.md` frontmatter 的 `name`，URI 最终段 MUST 等于它；
//! - 读取后 MUST 按条目 `resources[]` 的 sha256 digest 校验内容，MUST 把
//!   读到的 frontmatter 与条目 frontmatter **逐字段全量**比对（任何差异，
//!   含附加字段，MUST NOT load）；
//! - digest 校验失败（stale 信号）→ 经 `skills/get` 拉取当前条目快照重试
//!   一次（frontmatter/身份失败不是 stale，不触发）；
//! - listing 可为空/部分；名称不保证唯一，冲突 MUST 消歧、不得静默丢弃；
//! - 嵌套技能（祖先路径存在另一 SKILL.md）的 frontmatter 不得生效。

mod legacy_scan;
mod skills_list;
mod verify;

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::command::command_handler::CommandHandler;
use peri_acp_types::command::command_handler::CommandOutcome;
use peri_acp_types::command::command_route::CommandEntryKind;
use peri_acp_types::command::command_route::CommandLifecycle;
use peri_acp_types::command::command_route::CommandProvenance;
use peri_acp_types::command::command_route::CommandSource;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::command::CommandContext;
use peri_acp_types::command::CommandFeedback;
use peri_acp_types::command::CommandResult;
use peri_acp_types::command::FeedbackChannel;
use peri_acp_types::command::FeedbackLevel;
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::HandleToken;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::mcp_skills::SkillLookup;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::skills::SkillMetadata;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::client::McpClientHandle;
use skills_list as skills_list_functions;

pub(crate) use legacy_scan::{
    collect_skill_entries, is_skill_scheme, select_skill_resources, uri_eq_ignore_scheme_case,
};
/// W2 激活面复用（skill_activation）：正文读取、stale 恢复、frontmatter 校验。
pub(crate) use skills_list::{
    read_skill_resource_text, recover_entry_via_skills_get, refresh_entry_and_content,
    snapshot_via_skills_list, SkillResourceRead,
};
pub(crate) use verify::{
    frontmatter_maps_equal, parse_mcp_skill_md, parse_skill_frontmatter_map, verify_digest,
    verify_digest_bytes,
};

#[cfg(test)]
use legacy_scan::filter_nested_skills;
#[cfg(test)]
use peri_acp_types::mcp_skills::mcp_skill_name;
#[cfg(test)]
use peri_acp_types::skills::SkillOrigin;
#[cfg(test)]
use peri_acp_types::skills::SkillResource;
#[cfg(test)]
use peri_acp_types::skills::SkillSource;
#[cfg(test)]
use rmcp::model::Resource;
#[cfg(test)]
use rmcp::RoleClient;
#[cfg(test)]
use sha2::Sha256;
#[cfg(test)]
use skills_list::{
    entries_to_metadata, entry_from_dto, entry_to_metadata, SkillListEntry, SkillListEntryDto,
    SkillListResponse,
};
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use verify::{disambiguate_names, frontmatter_values_equal};

#[cfg(test)]
#[path = "skill_discovery_test.rs"]
mod tests;

// W4b/F6 收口：`core:{skill}` 投影的「宿主技能面关闭位」差分（投影函数级 +
// 管道级），由本模块统一挂载。
#[cfg(test)]
#[path = "skill_core_face_test.rs"]
mod core_face_tests;

#[path = "skill_discovery/discovery.rs"]
mod discovery;
pub(crate) use discovery::candidate_list;
#[cfg(test)]
use discovery::finish_command_source;
pub(crate) use discovery::mark_system_origins;
pub(crate) use discovery::mcp_namespace_reserved;
#[cfg(test)]
pub(crate) use discovery::mcp_route_entries;
pub(crate) use discovery::mcp_source_key;
pub(crate) use discovery::project_core_skill_commands;
#[cfg(test)]
pub(crate) use discovery::run_discovery;
pub(crate) use discovery::run_discovery_with_cache;
#[cfg(test)]
pub(crate) use discovery::McpSkillReleaser;
use discovery::MAX_LIST_PAGES;
use discovery::READ_CONCURRENCY;
use discovery::RESOURCE_READ_TIMEOUT;
use discovery::SKILLS_LIST_TIMEOUT;
