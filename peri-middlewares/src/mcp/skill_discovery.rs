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
use peri_acp_types::{
    command::command_handler::{CommandHandler, CommandOutcome},
    command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
    },
    command::{
        CommandContext, CommandFeedback, CommandResult, FeedbackChannel, FeedbackLevel,
        PromptStopReason,
    },
    command_registry::CommandRegistry,
    mcp_skills::{HandleToken, McpSkillRegistry},
    messages::BaseMessage,
    skills::SkillMetadata,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::client::McpClientHandle;
use skills_list::collect_via_skills_list;

pub(crate) use legacy_scan::{
    collect_skill_entries, is_skill_scheme, select_skill_resources, uri_eq_ignore_scheme_case,
};
pub(crate) use skills_list::refresh_entry_and_content;
pub(crate) use verify::{parse_mcp_skill_md, verify_digest, verify_digest_bytes};

#[cfg(test)]
use legacy_scan::filter_nested_skills;
#[cfg(test)]
use peri_acp_types::{
    mcp_skills::mcp_skill_name,
    skills::{SkillOrigin, SkillResource, SkillSource},
};
#[cfg(test)]
use rmcp::{model::Resource, RoleClient};
#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use skills_list::{
    entry_from_dto, verify_and_build, SkillListEntry, SkillListEntryDto, SkillListResponse,
    VerifyOutcome,
};
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use verify::{disambiguate_names, frontmatter_maps_equal, frontmatter_values_equal};

/// 单条资源读取超时。cancel 后悬挂窗口上界 = (N/8)×30s + 恢复条目×60s：
/// 恢复路径（skills/get + 重读）无 cancel 检查，仅外层两处检查——每个进入
/// 恢复的条目最多再悬挂 get 30s + 重读 30s。
const RESOURCE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 并发读取上限（tokio 现成原语，无新依赖）。
const READ_CONCURRENCY: usize = 8;
/// 单页 skills/list 请求超时。
const SKILLS_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// skills/list 分页游标不前进防御上限（防止 server 死循环返回同一游标）。
const MAX_LIST_PAGES: usize = 100;

/// 发现任务主体：规范/legacy 分流 → 并发读全文 → 解析/校验 → 回写 registry。
///
/// - legacy 且候选空 → 直接完成（空条目，静默）；
/// - peer 缺失 → warn + 完成（失败=空条目，不重试；重连才重扫）；
/// - cancel 触发 → 回退 Started 状态（不触发 on_change），下轮可重试。
///
/// 双注册表回写（决策 1 + A2）：`registry`（元数据面）完成后，若
/// `command_registry`（命令面）已装配，把发现结果经 [`mcp_route_entries`]
/// 转换后 `mark_source_completed`（来源键 = [`mcp_source_key`]，plugin
/// server 取末段，与 fullname 词法首段同构）；cancel 分支对齐
/// `clear_source_started`。None = 未装配命令面（print 模式/既有测试）→
/// 仅回写元数据面。
#[cfg(test)]
pub(crate) async fn run_discovery(
    registry: Arc<McpSkillRegistry>,
    command_registry: Option<Arc<CommandRegistry>>,
    handle: Arc<McpClientHandle>,
    handle_token: HandleToken,
    cancel: AgentCancellationToken,
) {
    run_discovery_with_cache(
        registry,
        command_registry,
        handle,
        handle_token,
        cancel,
        None,
    )
    .await;
}

pub(crate) async fn run_discovery_with_cache(
    registry: Arc<McpSkillRegistry>,
    command_registry: Option<Arc<CommandRegistry>>,
    handle: Arc<McpClientHandle>,
    handle_token: HandleToken,
    cancel: AgentCancellationToken,
    cache: Option<(crate::mcp::resource_cache::McpResourceCache, String)>,
) {
    // legacy 兜底：resources 里无 skill:// 候选 → 直接完成（规范模式不受
    // resources 影响——skills/list 是独立原语）。cancel 已触发时与下方
    // cancel 分支同构：回退 Started 状态（不触发 on_change），下轮可重试。
    if !handle.skills_capable && select_skill_resources(&handle.resources).is_empty() {
        if cancel.is_cancelled() {
            registry.clear_discovery_started(&handle.name, handle_token.clone());
            clear_command_source(&command_registry, &handle.name, handle_token);
            return;
        }
        registry.mark_discovery_completed(&handle.name, handle_token.clone(), vec![]);
        finish_command_source(
            &command_registry,
            &registry,
            &handle.name,
            handle_token,
            &[],
        );
        return;
    }
    let Some(peer) = handle.peer.clone() else {
        tracing::warn!(server = %handle.name, "MCP skill 发现：peer 缺失，跳过");
        registry.mark_discovery_completed(&handle.name, handle_token.clone(), vec![]);
        finish_command_source(
            &command_registry,
            &registry,
            &handle.name,
            handle_token,
            &[],
        );
        return;
    };
    let entries = if handle.skills_capable {
        match cache {
            Some((cache, origin)) => {
                skills_list::collect_via_skills_list_cached(
                    peer,
                    &handle.name,
                    cancel.clone(),
                    cache,
                    origin,
                )
                .await
            }
            None => collect_via_skills_list(peer, &handle.name, cancel.clone()).await,
        }
    } else {
        let candidates = select_skill_resources(&handle.resources);
        collect_skill_entries(peer, &handle.name, candidates, cancel.clone(), cache).await
    };
    if cancel.is_cancelled() {
        registry.clear_discovery_started(&handle.name, handle_token.clone());
        clear_command_source(&command_registry, &handle.name, handle_token);
        return;
    }
    if entries.0 && entries.1.is_empty() {
        tracing::warn!(
            server = %handle.name,
            "MCP skill 发现：候选非空但全部读取/校验失败，无可用条目",
        );
    }
    let skills = entries.1;
    finish_command_source(
        &command_registry,
        &registry,
        &handle.name,
        handle_token.clone(),
        &skills,
    );
    registry.mark_discovery_completed(&handle.name, handle_token, skills);
}

/// 命令面完成回写（决策 1 + A2）：来源键 = [`mcp_source_key`] 置 Discovered
/// 并注册转换后的 mcp 域 RouteEntry（冲突/越权条目由注册表纯拒绝 + warn，
/// 不整体回滚）。`command_registry = None` → no-op。
fn finish_command_source(
    command_registry: &Option<Arc<CommandRegistry>>,
    registry: &Arc<McpSkillRegistry>,
    server: &str,
    handle_token: HandleToken,
    skills: &[SkillMetadata],
) {
    if let Some(reg) = command_registry.as_ref() {
        // 审查 B1：保留词法域 server 不进命令面（防裸名污染/误删内置域）。
        if mcp_namespace_reserved(server) {
            tracing::warn!(
                server = %server,
                "MCP server 名与保留词法域冲突，跳过命令面注册（Skill 工具面不受影响）"
            );
            return;
        }
        reg.mark_source_completed(
            &mcp_source_key(server),
            handle_token,
            mcp_route_entries(registry, server, skills),
        );
    }
}

/// 命令面 cancel 回退（Phase 6 A3）：来源键 = [`mcp_source_key`] Started
/// 回退，下轮可重试。`command_registry = None` → no-op。
fn clear_command_source(
    command_registry: &Option<Arc<CommandRegistry>>,
    server: &str,
    handle_token: HandleToken,
) {
    if let Some(reg) = command_registry.as_ref() {
        // 审查 B1：保留词法域 server 从未在命令面 Started（finish 同源防护），
        // 回退为 no-op。
        if mcp_namespace_reserved(server) {
            return;
        }
        reg.clear_source_started(&mcp_source_key(server), handle_token);
    }
}

/// 命令面「来源键 = 注销前缀键 = fullname 词法首段域」的单一派生函数
/// （决策 1，替代 Phase 6 A3 的 `mcp:{末段}` 形态）：返回 server 名末段
/// 小写（纯 server 名即原名），与 [`mcp_route_entries`] 的 fullname 首段
/// 派生（[`mcp_namespace`]）同构——断连批量注销 `{末段}:` 前缀才能命中
/// fullname `{末段}:{skill}`。
///
/// plugin 提供的 server key 形如 `plugin:{plugin}:{server}` 时：来源键 =
/// `demosrv`、条目 fullname = `demosrv:beta`、注销前缀 `demosrv:` 三者
/// 一致；纯 server 名（demo）不变。
///
/// 衍生语义（设计风险行「同键 Conflict 拒绝」之外）：跨插件同名 server
/// （`plugin:pa:srvA` / `plugin:pb:srvA` 末段同为 `srvA`）共享来源键
/// `srvA`，后连者 Started 覆盖先连者、发现结果互相丢弃——与「取末段」
/// 既有决策（fullname 键 `srvA:*` 本就唯一，命令面注册同键冲突纯拒绝已
/// 接受此取舍）同一取舍，插件归属不可从命令名追溯。
pub(crate) fn mcp_source_key(server: &str) -> String {
    mcp_namespace(server)
}

/// 保留词法域防护（审查 B1）：server 名派生为保留域（core/ui/plugin/user/
/// mcp）时，命令面注册会产生 Level1 裸名路由污染（`core:hello` 登记裸名
/// `hello`）或断连批量注销误删内置域条目（前缀 `core:` 命中全部 core 域
/// 命令）。命令面在 [`finish_command_source`]/[`clear_command_source`] 与
/// [`run_ensure_discovery`] 命令面投影处整体跳过该 server；元数据面照常
/// 发现（SkillTool/SkillPreload 工具面仍可用）。
pub(crate) fn mcp_namespace_reserved(server: &str) -> bool {
    matches!(
        mcp_namespace(server).as_str(),
        "core" | "ui" | "plugin" | "user" | "mcp"
    )
}

/// server 名 → 命令面词法首段域（末段小写；纯 server 名不变）。
/// [`mcp_source_key`] 与 [`mcp_route_entries`] 共用（单一派生点）。
fn mcp_namespace(server: &str) -> String {
    server
        .rsplit_once(':')
        .map(|(_, s)| s)
        .unwrap_or(server)
        .to_lowercase()
}

/// mcp 域 RouteEntry 执行体（决策 A2/D：放行跳板，替代 Phase 6 A3 占位）。
///
/// - 交互式（拦截层，`ctx.supports_inject == true`）：`Inject(原文)` 放行
///   用户消息原文（含 `/server:skill` token）进 agent 管线，由
///   SkillPreloadMiddleware 完成 skill 注入——命令不被吞、技能全文在首轮
///   即见（与 `core:{skill}` 的 `AgentPassthrough` 同构）。
/// - RPC（`supports_inject == false`，execute-command 无 agent 管线）：
///   经 [`McpSkillRegistry::find_by_command`] 取 skill 全文并以
///   [`crate::skills::annotate_mcp_content`] 标注后直接返回（决策 D，
///   与预载注入内容同源）；内容缺失时回退 `Done + Info` 反馈。
///
/// 待 E2E 场景（e2e/ 现无 MCP skill fixture，不新建基础设施）：skill://
/// server → 会话建立（无消息）→ 面板 `/demo:hello` → 触发 → 管线内出现
/// `SkillTool(demo:hello)` ToolResult → Agent 回引用工具通路文案。
#[derive(Clone)]
pub(crate) struct McpSkillReleaser {
    registry: Arc<McpSkillRegistry>,
}

#[async_trait]
impl CommandHandler for McpSkillReleaser {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        if ctx.supports_inject {
            // 交互式：原文整段放行（含 `/` 前缀与 args）。原文缺失（理论
            // 不可达：拦截层恒透传）→ 回退 Done + Info，不吞命令不静默。
            if ctx.raw_text.is_empty() {
                return CommandOutcome::Done(CommandResult {
                    messages: ctx.history,
                    stop_reason: PromptStopReason::EndTurn,
                    feedback: Some(CommandFeedback {
                        level: FeedbackLevel::Info,
                        message:
                            "MCP skill 命令原文缺失，无法放行注入（请直接输入 /server:skill 触发）"
                                .to_string(),
                        channel: FeedbackChannel::UiOnly,
                    }),
                });
            }
            return CommandOutcome::Inject(ctx.raw_text);
        }
        // RPC：无管线可放行——直返 skill 全文 + 来源/工具通路标注（与
        // SkillPreload 注入内容同源：annotate_mcp_content）。命令名 =
        // raw_text 首 token（RPC 传命令名文本，剥 `/` 前缀容忍两种形态）。
        let name = ctx
            .raw_text
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        let hit = self
            .registry
            .find_by_command(name)
            .and_then(|meta| meta.content.clone().map(|content| (meta, content)));
        let (messages, feedback) = match hit {
            Some((meta, content)) => {
                let annotated = crate::skills::annotate_mcp_content(&meta, &content);
                let mut messages = ctx.history;
                // 全文追加为 human 消息，随 RPC 响应 messages 回传（Content-only）。
                messages.push(BaseMessage::human(annotated.clone()));
                let feedback = CommandFeedback {
                    level: FeedbackLevel::Info,
                    message: format!(
                        "MCP skill `{name}` 内容已返回（语义差异：交互式输入 `/{}` 走 preload 注入，RPC 直返全文）",
                        name
                    ),
                    channel: FeedbackChannel::UiOnly,
                };
                (messages, feedback)
            }
            None => {
                let feedback = CommandFeedback {
                    level: FeedbackLevel::Info,
                    message: format!(
                        "MCP skill `{name}` 内容未发现（server 未连接或未发现该 skill）；交互式输入 `/{}` 走 preload 注入",
                        name
                    ),
                    channel: FeedbackChannel::UiOnly,
                };
                (ctx.history, feedback)
            }
        };
        CommandOutcome::Done(CommandResult {
            messages,
            stop_reason: PromptStopReason::EndTurn,
            feedback: Some(feedback),
        })
    }
}

/// SkillMetadata → mcp 域 RouteEntry（决策 1 唯一转换点；词法：
/// `{server}:{skill}`，skill 名剥 `mcp__{server}__` 前缀）。
///
/// 首段 = server 名末段（[`mcp_namespace`]，与 [`mcp_source_key`] 同源
/// 派生）：plugin 提供的 server key 形如 `plugin:{plugin}:{server}`
/// （loader.rs:541），含冒号会突破词法 2 段上限 → 取末段；纯 server 名
/// （demo）不变。provenance = Mcp{server} + Discovered；handler =
/// [`McpSkillReleaser`]（决策 A2/D 放行跳板，持注册表供 RPC 直返全文）。
pub(crate) fn mcp_route_entries(
    registry: &Arc<McpSkillRegistry>,
    server: &str,
    skills: &[SkillMetadata],
) -> Vec<RouteEntry> {
    let namespace = mcp_namespace(server);
    let prefix = format!("mcp__{server}__");
    skills
        .iter()
        .filter_map(|s| {
            let skill = match s.name.strip_prefix(&prefix) {
                Some(skill) => skill.to_string(),
                None => {
                    tracing::warn!(
                        name = %s.name,
                        server = %server,
                        "mcp skill 名缺 mcp__{server}__ 前缀，跳过命令面注册"
                    );
                    return None;
                }
            };
            Some(RouteEntry {
                fullname: format!("{}:{}", namespace, skill.to_lowercase()),
                aliases: Vec::new(),
                description: s.description.clone(),
                kind: CommandEntryKind::McpSkill,
                category: None,
                args_schema: None,
                handler: Arc::new(McpSkillReleaser {
                    registry: Arc::clone(registry),
                }),
                provenance: CommandProvenance {
                    source: CommandSource::Mcp {
                        server: namespace.clone(),
                    },
                    lifecycle: CommandLifecycle::Discovered,
                },
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "skill_discovery_test.rs"]
mod tests;
