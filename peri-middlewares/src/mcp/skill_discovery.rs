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
    mcp_skills::{HandleToken, McpSkillRegistry, SkillLookup},
    messages::BaseMessage,
    skills::SkillMetadata,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::client::McpClientHandle;
use skills_list::collect_via_skills_list;

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
    entries_to_metadata, entry_from_dto, entry_to_metadata, SkillListEntry, SkillListEntryDto,
    SkillListResponse,
};
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use verify::{disambiguate_names, frontmatter_values_equal};

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
/// - legacy 且候选空 → 模板探测后再次检查取消，再完成合法空目录；
/// - peer 缺失或非空候选全部失败 → warn + Failed（重连才重扫）；
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
        // `#[cfg(test)]` 便捷入口：不承载宿主技能面关闭位（既有测试语义不变）。
        false,
    )
    .await;
}

/// legacy 边界探测（W2）：`resources/templates/list` 里是否存在 skill 形态的
/// 模板（`skill://…/SKILL.md`）。**只记录信号**：模板是模式、不能展开成条目，
/// 因此不注册候选；显式 URI 仍走 `skills/get` 与读取面的完整性校验路径。
/// 失败/超时按无模板处理（发现不因此失败）。
async fn probe_skill_templates(peer: &rmcp::service::Peer<rmcp::RoleClient>, server: &str) {
    let result = peri_time::timeout(SKILLS_LIST_TIMEOUT, peer.list_resource_templates(None)).await;
    let Ok(Ok(templates)) = result else {
        tracing::debug!(
            server,
            "MCP skill 发现：resources/templates/list 不可用，按无模板处理"
        );
        return;
    };
    let hit = templates.resource_templates.iter().find(|template| {
        let uri = template.uri_template.as_str();
        uri.len() >= 8 && uri[..8].eq_ignore_ascii_case("skill://") && uri.contains("SKILL.md")
    });
    if let Some(template) = hit {
        tracing::debug!(
            server,
            template = %template.uri_template,
            "MCP skill 发现：server 仅经模板暴露 skill 面（模板不可展开，显式 URI 仍可解析）"
        );
    }
}

pub(crate) async fn run_discovery_with_cache(
    registry: Arc<McpSkillRegistry>,
    command_registry: Option<Arc<CommandRegistry>>,
    handle: Arc<McpClientHandle>,
    handle_token: HandleToken,
    cancel: AgentCancellationToken,
    cache: Option<(crate::mcp::client::ConnectionResourceCache, String)>,
    // 宿主技能面关闭位（`BuiltinInstanceContext.skills_face_closed`，与 A24 关闭集
    // 同源）：真 ⇒ `core:{skill}` 裸名投影整体撤下（链槽关闭的配套半边）。
    // 由调用方从 pool 上下文读出经参数透传——发现任务不持有 pool。
    skills_face_closed: bool,
) {
    if cancel.is_cancelled() {
        registry.clear_discovery_started(&handle.name, handle_token.clone());
        clear_command_source(&command_registry, &handle.name, handle_token);
        return;
    }
    let Some(peer) = handle.peer.clone() else {
        tracing::warn!(server = %handle.name, "MCP skill 发现：peer 缺失，发现失败");
        registry.mark_discovery_failed(&handle.name, handle_token.clone());
        clear_command_source(&command_registry, &handle.name, handle_token);
        project_core_skill_commands(&command_registry, &registry, skills_face_closed);
        return;
    };
    // legacy 兜底：resources 里无 skill:// 候选 → 直接完成（规范模式不受
    // resources 影响——skills/list 是独立原语）。cancel 已触发时与下方
    // cancel 分支同构：回退 Started 状态（不触发 on_change），下轮可重试。
    //
    // 边界（W2）：候选空时查一次 `resources/templates/list`——server 可能只
    // 经模板暴露技能面（模板是模式不是具体资源，无法展开成条目）。有 skill
    // 形态模板时记录可发现性信号（显式 URI 仍可经 skills/get 或读取面
    // 完整性路径解析），**空列表本身是合法结果**，不报错、不重试。
    if !handle.skills_capable && select_skill_resources(&handle.resources).is_empty() {
        probe_skill_templates(&peer, &handle.name).await;
        if cancel.is_cancelled() {
            registry.clear_discovery_started(&handle.name, handle_token.clone());
            clear_command_source(&command_registry, &handle.name, handle_token);
            return;
        }
        registry.mark_discovery_completed(&handle.name, handle_token.clone(), vec![]);
        // W4b（F6）：元数据面完成后同批刷新 `core:{skill}` 裸名投影
        //（空条目 ⇒ 撤下既有 core 技能命令；宿主技能面关闭 ⇒ 同批撤下）。
        project_core_skill_commands(&command_registry, &registry, skills_face_closed);
        finish_command_source(
            &command_registry,
            &registry,
            &handle.name,
            handle_token,
            &[],
        );
        return;
    }
    let request_cache = cache.clone();
    let entries = if handle.skills_capable {
        let result = match cache {
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
        };
        match result {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(server = %handle.name, %error, "MCP skill discovery failed");
                if cancel.is_cancelled() {
                    registry.clear_discovery_started(&handle.name, handle_token.clone());
                } else {
                    registry.mark_discovery_failed(&handle.name, handle_token.clone());
                }
                clear_command_source(&command_registry, &handle.name, handle_token);
                project_core_skill_commands(&command_registry, &registry, skills_face_closed);
                return;
            }
        }
    } else {
        let candidates = select_skill_resources(&handle.resources);
        collect_skill_entries(peer, &handle.name, candidates, cancel.clone(), cache).await
    };
    if cancel.is_cancelled()
        || request_cache
            .as_ref()
            .is_some_and(|(cache, _)| !cache.is_current())
    {
        registry.clear_discovery_started(&handle.name, handle_token.clone());
        clear_command_source(&command_registry, &handle.name, handle_token);
        return;
    }
    if entries.0 && entries.1.is_empty() {
        tracing::warn!(
            server = %handle.name,
            "MCP skill 发现：候选非空但全部读取/校验失败，无可用条目",
        );
        registry.mark_discovery_failed(&handle.name, handle_token.clone());
        clear_command_source(&command_registry, &handle.name, handle_token);
        project_core_skill_commands(&command_registry, &registry, skills_face_closed);
        return;
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
    // W4b（F6）：元数据面写回**之后**再投影 core 裸名命令——投影读的是 registry
    // 当前状态，顺序反了会用上一代条目（首轮发现会得到空 core 面）。
    project_core_skill_commands(&command_registry, &registry, skills_face_closed);
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
        // 系统来源只经 core 域发布裸名命令。空集合仍完成该来源，清理此前
        // 版本留下的带前缀路由，并保留重连/断连的来源生命周期。
        let entries = if registry.is_system_origin(server) {
            Vec::new()
        } else {
            mcp_route_entries(registry, server, skills)
        };
        reg.mark_source_completed(&mcp_source_key(server), handle_token, entries);
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

/// 歧义候选的稳定渲染（`{server}:{skill}` 列表；消费面共用，禁止各自拼字符串）。
pub(crate) fn candidate_list(candidates: &[SkillMetadata]) -> String {
    let mut items: Vec<String> = candidates
        .iter()
        .map(|meta| match &meta.origin {
            // 用**完整 server key**（不是词法域末段）：候选本身必须是可输入、
            // 可唯一解析的完整名（`{server}:{skill}`；lookup_by_command 接受
            // 完整 server 名或末段，完整名消歧无歧义）。
            Some(peri_acp_types::skills::SkillOrigin::Mcp { server, .. }) => {
                let bare = peri_acp_types::mcp_skills::bare_skill_segment(&meta.name)
                    .unwrap_or(meta.name.as_str())
                    .to_string();
                format!("{server}:{bare}")
            }
            _ => meta.name.clone(),
        })
        .collect();
    items.sort();
    items.dedup();
    items.join(", ")
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
        let (messages, feedback) = match self.registry.lookup_by_command(name) {
            SkillLookup::Ambiguous(candidates) => {
                // 多 origin 命中同一命令形态：显式拒绝并列出候选（不静默取首个）。
                let list = candidate_list(&candidates);
                tracing::warn!(command = %name, candidates = %list, "MCP skill 命令命中多个 origin，拒绝并列出候选");
                let feedback = CommandFeedback {
                    level: FeedbackLevel::Info,
                    message: format!("MCP skill `{name}` 命中多个来源，请用完整名消歧：{list}"),
                    channel: FeedbackChannel::UiOnly,
                };
                (ctx.history, feedback)
            }
            SkillLookup::Found(meta) => {
                // W2：正文一律经统一 activation（resources/read + digest/frontmatter
                // 校验；stale 经 skills/get 刷新一次）；失败不注入，不回落缓存。
                match crate::mcp::skill_activation::activate(&self.registry, &meta, None).await {
                    Ok(content) => {
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
                    Err(error) => {
                        let feedback = CommandFeedback {
                            level: FeedbackLevel::Info,
                            message: format!(
                                "MCP skill `{name}` 激活失败（{}）；交互式输入 `/{}` 走 preload 注入",
                                error.reason(),
                                name
                            ),
                            channel: FeedbackChannel::UiOnly,
                        };
                        (ctx.history, feedback)
                    }
                }
            }
            SkillLookup::Missing => {
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

// ─── core 域裸名命令投影（W4b / F6）────────────────────────────────────────

/// `core:{skill}` 裸名命令的 handler：与 `AgentPassthrough` 同语义——
/// 交互式把用户原文（含 `/skill-name` token）整段交还 agent 管线，由
/// `SkillPreloadMiddleware` 检测并注入全文；RPC 路径（execute-command，
/// 无 agent 管线）由调用方显式报错，不在这里自行返回内容。
///
/// 正文读取不在 handler 内进行：`/skill` 的正文一律走统一 activation
/// （J1/X1：用户显式 token 触发 → 全文，但读取路径与 SkillTool 同源）。
#[derive(Clone)]
pub(crate) struct CoreSkillPassthrough;

#[async_trait]
impl CommandHandler for CoreSkillPassthrough {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Inject(ctx.raw_text)
    }
}

/// SkillMetadata → `core:{skill}` 裸名 RouteEntry（W4b/F6）。
///
/// 裸名 = `mcp__{server}__{skill}` 的末段（[`bare_skill_segment`]），
/// 与 `/skill-name` 的用户输入口径一致；kind = `Skill`、provenance =
/// `Core` + Connected（UX 与既有的本地 skill 命令面逐位相同，只是来源
/// 改成 MCP 侧投影）。
///
/// frontmatter `aliases`（wire 不改写，X3）派生为命令别名——宿主从 metadata
/// 派生 CLI 别名，不依赖任何本地扫描。
pub(crate) fn core_route_entries(skills: &[SkillMetadata]) -> Vec<RouteEntry> {
    skills
        .iter()
        .filter_map(|meta| {
            let bare = peri_acp_types::mcp_skills::bare_skill_segment(&meta.name)
                .unwrap_or(meta.name.as_str())
                .to_lowercase();
            if bare.is_empty() {
                return None;
            }
            Some(RouteEntry {
                fullname: format!("core:{bare}"),
                aliases: frontmatter_aliases(meta),
                description: meta.description.clone(),
                kind: CommandEntryKind::Skill,
                category: None,
                args_schema: None,
                handler: Arc::new(CoreSkillPassthrough),
                provenance: CommandProvenance {
                    source: CommandSource::Core,
                    lifecycle: CommandLifecycle::Connected,
                },
            })
        })
        .collect()
}

/// 发现条目 frontmatter 的 `aliases`（verbatim map；非字符串项忽略）。
fn frontmatter_aliases(meta: &SkillMetadata) -> Vec<String> {
    meta.frontmatter
        .as_ref()
        .and_then(|fm| fm.get("aliases"))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .filter(|alias| !alias.is_empty() && !alias.contains(':'))
                .collect()
        })
        .unwrap_or_default()
}

/// 当前已投影的 `core:` 技能命令全名（reconcile 的 stale 集）。
///
/// 只取 `kind = Skill` + `source = Core` 的条目：内置命令（compact/clear/…）
/// 与插件命令的 kind 不同，因此不会被本投影误撤。
fn registered_core_skill_commands(command_registry: &CommandRegistry) -> Vec<String> {
    command_registry
        .snapshot()
        .iter()
        .filter(|entry| {
            entry.kind == CommandEntryKind::Skill
                && matches!(entry.provenance.source, CommandSource::Core)
        })
        .map(|entry| entry.fullname.clone())
        .collect()
}

/// 把系统来源（builtin `workspace` 等本机受信实例）的技能投影为 `core:{skill}`
/// 裸名命令（W4b/F6；只读 registry metadata，不读正文）。
///
/// 语义：
/// - 幂等——`reconcile(stale, new)` 单次写锁内完成「撤旧 + 注册新」，内容无变化
///   时不触发 on_change（不会造成命令面板抖动）；
/// - 来源集合 = `registry.system_skills()`（由 [`run_ensure_discovery`] 按连接事实
///   标注；实例关闭/断连 ⇒ 集合自然为空 ⇒ 命令同批撤下，X4）；
/// - `skills_face_closed`（宿主技能面关闭位，与 A24 关闭集同源）= 真 ⇒ 目标集合为
///   空：**仍执行** `reconcile` 以撤下既有条目（链槽关闭不留幽灵路由）；
/// - 冲突（同名内置命令 / 已存在的 core 条目）按注册表既有纯拒绝语义跳过并告警。
pub(crate) fn project_core_skill_commands(
    command_registry: &Option<Arc<CommandRegistry>>,
    registry: &Arc<McpSkillRegistry>,
    skills_face_closed: bool,
) {
    let Some(command_registry) = command_registry.as_ref() else {
        return;
    };
    let stale = registered_core_skill_commands(command_registry);
    let entries = if skills_face_closed {
        Vec::new()
    } else {
        core_route_entries(&registry.system_skills())
    };
    let (removed, added) = command_registry.reconcile(&stale, entries);
    if removed + added > 0 {
        tracing::debug!(
            removed,
            added,
            "core 域技能命令投影已刷新（MCP registry → /skill-name）"
        );
    }
}

/// 发现任务 spawn 前的来源标注：把**系统来源**（host 绑定的 builtin 实例，
/// `ConfigSource::Builtin`；不含被 A24 关闭集关闭的实例）的 server 键记入
/// registry，供摘要投递（J1）与 `core:` 命令投影（F6）判定。
///
/// 身份来自连接事实而不是名字自称（X6/X7）：外部 server 即使叫 `workspace`
/// 也不满足 `ConfigSource::Builtin`。
pub(crate) fn mark_system_origins(
    registry: &Arc<McpSkillRegistry>,
    handles: &[Arc<McpClientHandle>],
    closed: &std::collections::BTreeSet<String>,
) {
    let names: Vec<String> = handles
        .iter()
        .filter(|handle| {
            (matches!(
                handle.source.as_ref(),
                Some(crate::mcp::config::ConfigSource::Builtin { .. })
            ) || matches!(
                handle.source.as_ref(),
                Some(crate::mcp::config::ConfigSource::WorkspaceRemote)
            )) && !super::builtin::is_closed_source(&handle.name, handle.source.as_ref(), closed)
        })
        .map(|handle| handle.name.clone())
        .collect();
    registry.mark_system_origins(&names);
}

#[cfg(test)]
#[path = "skill_discovery_test.rs"]
mod tests;

// W4b/F6 收口：`core:{skill}` 投影的「宿主技能面关闭位」差分（投影函数级 +
// 管道级），由本模块统一挂载。
#[cfg(test)]
#[path = "skill_core_face_test.rs"]
mod core_face_tests;
