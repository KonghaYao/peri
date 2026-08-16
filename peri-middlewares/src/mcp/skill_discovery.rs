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

use std::{path::PathBuf, sync::Arc};

use gray_matter::{engine::YAML, Matter};
use peri_acp_types::{
    mcp_skills::{mcp_skill_name, HandleToken, McpSkillRegistry},
    skills::{SkillMetadata, SkillOrigin, SkillResource, SkillSource},
};
use rmcp::{
    model::{
        ClientRequest, CustomRequest, ReadResourceRequestParams, Resource, ResourceContents,
        ServerResult,
    },
    Peer, RoleClient,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::client::McpClientHandle;

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
pub(crate) async fn run_discovery(
    registry: Arc<McpSkillRegistry>,
    handle: Arc<McpClientHandle>,
    handle_token: HandleToken,
    cancel: AgentCancellationToken,
) {
    // legacy 兜底：resources 里无 skill:// 候选 → 直接完成（规范模式不受
    // resources 影响——skills/list 是独立原语）。
    if !handle.skills_capable && select_skill_resources(&handle.resources).is_empty() {
        registry.mark_discovery_completed(&handle.name, handle_token, vec![]);
        return;
    }
    let Some(peer) = handle.peer.clone() else {
        tracing::warn!(server = %handle.name, "MCP skill 发现：peer 缺失，跳过");
        registry.mark_discovery_completed(&handle.name, handle_token, vec![]);
        return;
    };
    let entries = if handle.skills_capable {
        collect_via_skills_list(peer, &handle.name, cancel.clone()).await
    } else {
        let candidates = select_skill_resources(&handle.resources);
        collect_skill_entries(peer, &handle.name, candidates, cancel.clone()).await
    };
    if cancel.is_cancelled() {
        registry.clear_discovery_started(&handle.name, handle_token);
        return;
    }
    if entries.0 && entries.1.is_empty() {
        tracing::warn!(
            server = %handle.name,
            "MCP skill 发现：候选非空但全部读取/校验失败，无可用条目",
        );
    }
    registry.mark_discovery_completed(&handle.name, handle_token, entries.1);
}

// ─── SEP-2640 规范路径：skills/list ───────────────────────────────────────

/// `skills/list` 响应（分页；`nextCursor` 缺省兼容未分页 server）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillListResponse {
    #[serde(default)]
    skills: Vec<SkillListEntryDto>,
    #[serde(default)]
    next_cursor: Option<String>,
}

/// `skills/list` 条目（`frontmatter` 为 SKILL.md YAML frontmatter 的
/// verbatim JSON 渲染——规范要求原样透传，非精选子集）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillListEntryDto {
    uri: String,
    frontmatter: serde_json::Map<String, serde_json::Value>,
    /// 完整资源清单 {uri, digest}；动态生成技能可省略（规范 MAY）。用
    /// `Option` 区分省略（None = 动态技能）与显式空数组/部分清单
    /// （Some——present 时必须完整，含 SKILL.md 自身条目）。
    #[serde(default)]
    resources: Option<Vec<SkillResourceDto>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillResourceDto {
    uri: String,
    digest: String,
}

/// `skills/get` 响应：`skill` 字段与 skills/list 条目同构（相同字段与规则），
/// 是该技能当前条目快照。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillGetResponse {
    skill: SkillListEntryDto,
}

/// 解析后的技能条目（frontmatter 原样保留——逐字段全量比对用；name/
/// description 只在 `entry_from_dto` 作必填门闩，不单独存字段）。
#[derive(Debug, Clone)]
struct SkillListEntry {
    uri: String,
    /// SKILL.md YAML frontmatter 的 verbatim JSON 渲染（非精选子集）。
    /// `verify_and_build` 以此与读到的 frontmatter 全等比对。
    frontmatter: serde_json::Map<String, serde_json::Value>,
    /// 完整资源清单；`None` = 省略（动态生成技能，规范 MAY）——接受但
    /// 无法内容绑定；`Some(..)` = 显式声明（present 时必须完整，含
    /// SKILL.md 自身条目，否则完整性违规拒绝）。
    resources: Option<Vec<SkillResource>>,
}

/// 纯函数：DTO → 条目。frontmatter 缺 name/description（Agent Skills
/// 规范要求必填，缺失即非规范）→ None（该条目跳过）；frontmatter map
/// 原样移入（verbatim，不 clone 消耗）。
fn entry_from_dto(dto: SkillListEntryDto) -> Option<SkillListEntry> {
    dto.frontmatter.get("name")?.as_str()?;
    dto.frontmatter.get("description")?.as_str()?;
    Some(SkillListEntry {
        uri: dto.uri,
        frontmatter: dto.frontmatter,
        resources: dto.resources.map(|rs| {
            rs.into_iter()
                .map(|r| SkillResource {
                    uri: r.uri,
                    digest: r.digest,
                })
                .collect()
        }),
    })
}

/// 规范路径：`skills/list` 分页枚举 → 每条目 `resources/read` 读 SKILL.md →
/// digest 校验 → frontmatter 逐字段比对 → 注册。
///
/// 返回 `(need_summary_warn, entries)`：条目非空时恒为 `(false, _)`；
/// 空列表/调用失败 → `(false, 空)`（空列表合法——listing 可为空/部分，
/// 调用失败已各自 warn）；候选非空但全部校验失败 → `(true, 空)`。
async fn collect_via_skills_list(
    peer: Peer<RoleClient>,
    server: &str,
    cancel: AgentCancellationToken,
) -> (bool, Vec<SkillMetadata>) {
    let mut dto_entries: Vec<SkillListEntryDto> = Vec::new();
    let mut cursor: Option<String> = None;
    for _page in 0..MAX_LIST_PAGES {
        if cancel.is_cancelled() {
            return (false, Vec::new());
        }
        let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
        let request = ClientRequest::CustomRequest(CustomRequest::new("skills/list", params));
        let response = tokio::time::timeout(SKILLS_LIST_TIMEOUT, peer.send_request(request)).await;
        let page: SkillListResponse = match response {
            Ok(Ok(ServerResult::CustomResult(custom))) => match custom.result_as() {
                Ok(page) => page,
                Err(err) => {
                    tracing::warn!(server, error = %err, "MCP skill 发现：skills/list 响应解析失败");
                    return (false, Vec::new());
                }
            },
            Ok(Ok(_)) => {
                tracing::warn!(server, "MCP skill 发现：skills/list 返回非预期响应类型");
                return (false, Vec::new());
            }
            Ok(Err(err)) => {
                tracing::warn!(server, error = %err, "MCP skill 发现：skills/list 调用失败");
                return (false, Vec::new());
            }
            Err(_) => {
                tracing::warn!(
                    server,
                    "MCP skill 发现：skills/list 超时 ({}s)",
                    SKILLS_LIST_TIMEOUT.as_secs()
                );
                return (false, Vec::new());
            }
        };
        dto_entries.extend(page.skills);
        let next = page.next_cursor;
        if next.as_ref().is_some() && next.as_ref() == cursor.as_ref() {
            tracing::warn!(server, "MCP skill 发现：skills/list 游标不前进，终止分页");
            break;
        }
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    if dto_entries.is_empty() {
        // 空列表合法（listing 可为空/部分），静默。
        return (false, Vec::new());
    }

    let entries: Vec<SkillListEntry> = dto_entries.into_iter().filter_map(entry_from_dto).collect();
    if entries.is_empty() {
        tracing::warn!(
            server,
            "MCP skill 发现：skills/list 条目全部缺 frontmatter name/description"
        );
        return (false, Vec::new());
    }
    let out = fetch_and_verify(peer, server, entries, cancel).await;
    (out.is_empty(), out)
}

/// 并发读取并校验条目（Semaphore + JoinSet）：每条任务持 peer clone；
/// digest 校验 / frontmatter 比对 / URI 最终段校验任一失败 → 条目过滤。
async fn fetch_and_verify(
    peer: Peer<RoleClient>,
    server: &str,
    entries: Vec<SkillListEntry>,
    cancel: AgentCancellationToken,
) -> Vec<SkillMetadata> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(READ_CONCURRENCY));
    let server_owned = server.to_string();
    let mut join_set = tokio::task::JoinSet::new();
    for entry in entries {
        let permit = Arc::clone(&semaphore);
        let task_peer = peer.clone();
        let task_server = server_owned.clone();
        let task_cancel = cancel.clone();
        join_set.spawn(async move {
            if task_cancel.is_cancelled() {
                return None;
            }
            let _permit = permit.acquire_owned().await;
            fetch_and_verify_one(task_peer, &task_server, entry, task_cancel).await
        });
    }
    let mut out = Vec::with_capacity(join_set.len());
    while let Some(joined) = join_set.join_next().await {
        if cancel.is_cancelled() {
            // 提前退出：cancel 后不再等剩余任务（缩短 Started 悬挂窗口）。
            join_set.abort_all();
            break;
        }
        if let Ok(Some(meta)) = joined {
            out.push(meta);
        }
    }
    // JoinSet 完成序非确定：按 name 排序保证输出确定（下游 registry 条目序/
    // commands 列表随之确定）。
    out.sort_by(|a, b| a.name.cmp(&b.name));
    disambiguate_names(server, out)
}

/// 单条目：`resources/read` → `verify_and_build`；digest 校验失败（stale
/// 信号）→ `skills/get` 拉取当前条目快照重试一次（uri 核对 → 重新读全文 →
/// 重新校验）；仍失败 → 拒绝（恢复失败日志在恢复流程内发出）。
async fn fetch_and_verify_one(
    peer: Peer<RoleClient>,
    server: &str,
    entry: SkillListEntry,
    cancel: AgentCancellationToken,
) -> Option<SkillMetadata> {
    if cancel.is_cancelled() {
        return None;
    }
    let uri = entry.uri.clone();
    // 发现侧首读：SKILL.md 必须是 Text（Blob/失败/超时均无法校验 frontmatter
    // 与 digest）→ 条目过滤。
    let SkillResourceRead::Text(text, _) = read_skill_resource_text(&peer, server, &uri).await
    else {
        return None;
    };
    match verify_and_build(server, &entry, &text) {
        VerifyOutcome::Built(meta) => Some(meta),
        VerifyOutcome::Rejected => None,
        VerifyOutcome::DigestMismatch => {
            // digest 不匹配 = 内容陈旧/被替换（规范建议：经 skills/get 恢复）；
            // frontmatter 比对失败 / URI 身份失败不是 stale 信号，不恢复。
            if cancel.is_cancelled() {
                return None;
            }
            tracing::debug!(
                server,
                %uri,
                "MCP skill digest 校验失败，尝试 skills/get 拉取当前条目快照"
            );
            let meta = recover_via_skills_get(&peer, server, &uri).await;
            if cancel.is_cancelled() {
                return None;
            }
            meta
        }
    }
}

/// 共享恢复流程：`skills/get` 拉取当前条目快照 → 响应 uri 与请求核对
/// （不一致 = server 违规，拒绝恢复）→ 按新条目重读 SKILL.md 全文 →
/// digest + frontmatter 全量校验。发现侧（digest stale 恢复）与读取面
/// （resource_tool 热更新恢复）共用。
///
/// 恢复失败均记录日志（真实拒绝）；成功仅 debug（恢复是自愈路径，不告警）。
async fn recover_via_skills_get(
    peer: &Peer<RoleClient>,
    server: &str,
    uri: &str,
) -> Option<SkillMetadata> {
    let refreshed = fetch_skill_entry(peer, uri).await?;
    // scheme 大小写不敏感核对（RFC 3986，与读取面判定一致）——请求 uri
    // 大写 scheme 时恢复路径不再错误失败。
    if !uri_eq_ignore_scheme_case(&refreshed.uri, uri) {
        tracing::warn!(
            server,
            %uri,
            "MCP skill skills/get 返回 uri 与请求不一致，拒绝恢复"
        );
        return None;
    }
    // 新条目快照：以新条目的 SKILL.md uri 重新读全文（resources 可能已
    // 变化），再走完整校验（digest / frontmatter / 身份）。若新条目 digest
    // 与旧条目相同，重读内容必再次校验失败——由 verify 兜底，无需显式比较。
    let new_text = match read_skill_resource_text(peer, server, &refreshed.uri).await {
        SkillResourceRead::Text(text, _) => text,
        SkillResourceRead::NotText => {
            tracing::warn!(
                server,
                %uri,
                "MCP skill 恢复失败：重读 SKILL.md 未返回文本内容（Blob 资源不支持恢复），拒绝加载"
            );
            return None;
        }
        SkillResourceRead::Failed => {
            tracing::warn!(
                server,
                %uri,
                "MCP skill 恢复失败：按新条目重读 SKILL.md 失败/超时，拒绝加载"
            );
            return None;
        }
    };
    match verify_and_build(server, &refreshed, &new_text) {
        VerifyOutcome::Built(meta) => Some(meta),
        VerifyOutcome::DigestMismatch | VerifyOutcome::Rejected => {
            tracing::warn!(
                server,
                %uri,
                "MCP skill digest 校验失败，skills/get 恢复失败（新条目仍校验不通过），拒绝加载"
            );
            None
        }
    }
}

/// 读取面热更新恢复（resource_tool 专用，pub(crate)）：digest 不匹配 /
/// 未列出时经 `skills/get` 刷新单条目并重新读取请求 uri 内容。
///
/// 1. `skills/get` 拉取覆盖条目（entry_uri）的当前快照（响应 uri 核对，
///    scheme 大小写不敏感）；
/// 2. 按新条目重读 SKILL.md 并全量校验（digest + frontmatter + 身份）；
/// 3. 重新读请求 uri 内容：与 SKILL.md 相同则复用已校验文本；否则按新
///    条目 resources 中该 uri 的 digest 校验（热更新可能已列出新文件）。
///
/// **恢复路径仅支持 Text**：Blob 附属资源 digest 校验失败 → 恢复失败
/// （拒绝）——重读无 Text 内容时按 [`SkillResourceRead::NotText`] 区分文案
/// （"未返回文本内容"，而非误报"失败/超时"）。
///
/// 成功 → `Some((新条目 metadata, 请求 uri 内容, mime))`（调用方回写
/// registry）；任一失败 → `None`。恢复只尝试一次；get/read 各 30s 超时
/// （复用 SKILLS_LIST_TIMEOUT / RESOURCE_READ_TIMEOUT）：Unlisted 分支
/// 恢复上界 ≤90s（get 30s + SKILL.md 重读 30s + 附属重读 30s），Listed
/// 分支 = 首读 120s + 恢复 ≤90s ≈ 210s；读取面工具无整体超时
/// （timeout()=None），悬挂受 agent 层控制。
pub(crate) async fn refresh_entry_and_content(
    peer: &Peer<RoleClient>,
    server: &str,
    entry_uri: &str,
    request_uri: &str,
) -> Option<(SkillMetadata, String, Option<String>)> {
    let meta = recover_via_skills_get(peer, server, entry_uri).await?;
    if request_uri == entry_uri {
        // SKILL.md 内容已在恢复流程中校验过，直接复用（mime 未知 → None，
        // 调用方格式化时取默认值）。
        let content = meta.content.clone()?;
        return Some((meta, content, None));
    }
    // 附属资源：按新条目 resources 重新定位 digest 校验（scheme 大小写
    // 不敏感，与读取面判定一致）。
    let Some(expected) = meta
        .resources
        .iter()
        .find(|r| uri_eq_ignore_scheme_case(&r.uri, request_uri))
    else {
        tracing::warn!(
            server,
            %request_uri,
            "MCP skill 热更新恢复失败：新条目未列出请求 uri，拒绝加载"
        );
        return None;
    };
    let text = match read_skill_resource_text(peer, server, request_uri).await {
        SkillResourceRead::Text(text, mime) => {
            if !verify_digest(&text, &expected.digest) {
                tracing::warn!(
                    server,
                    %request_uri,
                    "MCP skill 热更新恢复失败：请求内容 digest 与新条目不一致，拒绝加载"
                );
                return None;
            }
            (text, mime)
        }
        SkillResourceRead::NotText => {
            // 与"失败/超时"区分：内容是 Blob（恢复路径仅支持 Text），
            // 不是传输层错误。
            tracing::warn!(
                server,
                %request_uri,
                "MCP skill 热更新恢复失败：恢复重读未返回文本内容（Blob 资源不支持恢复），拒绝加载"
            );
            return None;
        }
        SkillResourceRead::Failed => {
            tracing::warn!(
                server,
                %request_uri,
                "MCP skill 热更新恢复失败：重新读取请求资源失败/超时，拒绝加载"
            );
            return None;
        }
    };
    Some((meta, text.0, text.1))
}

/// 单次 `resources/read` 的读取结果三态：成功 Text（携带文本与 mime）、
/// RPC 失败/超时、响应成功但无 Text 内容（Blob 资源）。区分后两者供恢复
/// 路径给出准确文案（NotText 不是传输层错误）。
enum SkillResourceRead {
    /// Text 内容（文本 + mime；mime 可能缺省）
    Text(String, Option<String>),
    /// RPC 失败/超时
    Failed,
    /// 响应成功但无 Text 内容（Blob 资源——恢复路径仅支持 Text）
    NotText,
}

/// 单次 `resources/read`：读 SKILL.md 文本（30s 超时；失败/超时 →
/// [`SkillResourceRead::Failed`] + debug；响应无 Text → [`SkillResourceRead::NotText`]）。
async fn read_skill_resource_text(
    peer: &Peer<RoleClient>,
    server: &str,
    uri: &str,
) -> SkillResourceRead {
    let request = ReadResourceRequestParams::new(uri.to_string());
    let result =
        match tokio::time::timeout(RESOURCE_READ_TIMEOUT, peer.read_resource(request)).await {
            Ok(Ok(result)) => result,
            Ok(Err(err)) => {
                tracing::debug!(server, %uri, "MCP skill 资源读取失败: {err}");
                return SkillResourceRead::Failed;
            }
            Err(_) => {
                tracing::debug!(
                    server,
                    %uri,
                    "MCP skill 资源读取超时 ({}s)",
                    RESOURCE_READ_TIMEOUT.as_secs()
                );
                return SkillResourceRead::Failed;
            }
        };
    result
        .contents
        .iter()
        .find_map(|content| match content {
            ResourceContents::TextResourceContents { text, mime_type, .. } => {
                Some(SkillResourceRead::Text(text.clone(), mime_type.clone()))
            }
            _ => None,
        })
        .unwrap_or(SkillResourceRead::NotText)
}

/// `skills/get` 客户端能力（规范：声明扩展的 server MUST 实现）：
/// 请求 `{"uri": skill://.../SKILL.md}`，响应 `{"skill": {uri, frontmatter,
/// resources}}`——与 skills/list 条目同构（相同字段与规则）。URI 非技能 →
/// 错误 -32602；未列举技能也须应答；结果是对应技能当前条目快照。
/// 失败/超时/解析失败 → None + warn。
///
/// 私有：发现侧（digest stale 恢复）与读取面热更新恢复（`refresh_entry_
/// and_content`）共用——读取面不直接触碰 `SkillListEntry`。
async fn fetch_skill_entry(peer: &Peer<RoleClient>, uri: &str) -> Option<SkillListEntry> {
    let params = serde_json::json!({ "uri": uri });
    let request = ClientRequest::CustomRequest(CustomRequest::new("skills/get", Some(params)));
    let response = tokio::time::timeout(SKILLS_LIST_TIMEOUT, peer.send_request(request)).await;
    let parsed: SkillGetResponse = match response {
        Ok(Ok(ServerResult::CustomResult(custom))) => match custom.result_as() {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(%uri, error = %err, "MCP skill skills/get 响应解析失败");
                return None;
            }
        },
        Ok(Ok(_)) => {
            tracing::warn!(%uri, "MCP skill skills/get 返回非预期响应类型");
            return None;
        }
        Ok(Err(err)) => {
            tracing::warn!(%uri, error = %err, "MCP skill skills/get 调用失败");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                %uri,
                "MCP skill skills/get 超时 ({}s)",
                SKILLS_LIST_TIMEOUT.as_secs()
            );
            return None;
        }
    };
    entry_from_dto(parsed.skill)
}

/// 校验结果：区分 digest 失败（stale 信号，可经 skills/get 恢复）与其他
/// 失败（frontmatter 比对 / URI 身份失败，非 stale，不可恢复）。
#[derive(Debug)]
enum VerifyOutcome {
    /// 校验通过，metadata 构建成功
    Built(SkillMetadata),
    /// digest 不匹配（内容与条目承诺不一致）
    DigestMismatch,
    /// frontmatter 比对 / URI 身份 / frontmatter 解析 / 完整性违规
    Rejected,
}

/// 纯函数：digest 校验 → frontmatter 逐字段全量比对 → 身份/构建。
///
/// - resources 省略（None，动态生成技能）→ 接受但无法内容绑定（debug 标注）；
/// - resources present（Some）但不含 SKILL.md 自身条目（含显式空数组）→
///   `Rejected`（完整性 MUST：present 时必须完整）；
/// - digest 不匹配 → `DigestMismatch`（内容与条目承诺不一致，规范 MUST NOT
///   use；stale 信号，调用方按需走 skills/get 恢复——纯函数内只 debug，
///   恢复失败才由恢复流程告警）；
/// - frontmatter 与条目**逐字段全量**不一致（含附加字段如 license/metadata
///   的任何差异）→ `Rejected`（陈旧或被篡改，规范 MUST NOT load）。
fn verify_and_build(server: &str, entry: &SkillListEntry, content: &str) -> VerifyOutcome {
    match &entry.resources {
        None => {
            tracing::debug!(
                server,
                uri = %entry.uri,
                "MCP skill 条目省略 resources（动态生成技能），无法内容绑定"
            );
        }
        Some(resources) => {
            // 完整性 MUST：present 时必须完整，含 SKILL.md 自身条目。
            let Some(digest) = resources
                .iter()
                .find(|r| r.uri == entry.uri)
                .map(|r| r.digest.as_str())
            else {
                tracing::warn!(
                    server,
                    uri = %entry.uri,
                    "MCP skill resources 未含 SKILL.md 自身条目（完整性违规），拒绝加载"
                );
                return VerifyOutcome::Rejected;
            };
            if !verify_digest(content, digest) {
                tracing::debug!(
                    server,
                    uri = %entry.uri,
                    "MCP skill digest 校验失败：内容与条目声明不一致（stale 信号，尝试 skills/get 恢复）"
                );
                return VerifyOutcome::DigestMismatch;
            }
        }
    }
    let Some(fm_map) = parse_skill_frontmatter_map(content) else {
        tracing::warn!(
            server,
            uri = %entry.uri,
            "MCP skill frontmatter 解析失败（YAML 非法或缺失），拒绝加载"
        );
        return VerifyOutcome::Rejected;
    };
    if !frontmatter_maps_equal(&fm_map, &entry.frontmatter) {
        tracing::warn!(
            server,
            uri = %entry.uri,
            "MCP skill frontmatter 与 skills/list 条目不一致（陈旧或篡改），拒绝加载"
        );
        return VerifyOutcome::Rejected;
    }
    // name/description 必填（Agent Skills 规范）；frontmatter 全等已保证与
    // 条目一致，这里仅作内容侧门闩。
    let Some(name) = fm_map.get("name").and_then(|v| v.as_str()) else {
        tracing::warn!(
            server,
            uri = %entry.uri,
            "MCP skill frontmatter 缺 name，拒绝加载"
        );
        return VerifyOutcome::Rejected;
    };
    let Some(description) = fm_map.get("description").and_then(|v| v.as_str()) else {
        tracing::warn!(
            server,
            uri = %entry.uri,
            "MCP skill frontmatter 缺 description，拒绝加载"
        );
        return VerifyOutcome::Rejected;
    };
    match build_metadata(
        server,
        &entry.uri,
        name,
        description,
        content,
        entry.resources.clone().unwrap_or_default(),
    ) {
        Some(meta) => VerifyOutcome::Built(meta),
        None => VerifyOutcome::Rejected,
    }
}

// ─── legacy 兼容路径：resources 扫描（Claude Code 早期生态）───────────────

/// 纯函数：`skill://` scheme 前缀判定（RFC 3986 scheme 大小写不敏感；
/// `Skill://`/`SKILL://` 均命中）。发现侧（resources 扫描 / URI 段解析）与
/// 读取面（resource_tool 完整性绑定）共用。
pub(crate) fn is_skill_scheme(uri: &str) -> bool {
    uri.get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("skill://"))
}

/// 纯函数：URI 相等比较，scheme 部分（前 8 字符）大小写不敏感（RFC 3986
/// scheme 不区分大小写；authority/path 仍按原样敏感）。两入参均须为
/// `skill://` 形态（长度 ≥ 8）——读取面（resource_tool 完整性绑定）与
/// 发现侧恢复路径（skills/get uri 核对 / digest 查找）共用，保证请求侧
/// 大写 scheme 时恢复路径与读取面判定一致。
pub(crate) fn uri_eq_ignore_scheme_case(a: &str, b: &str) -> bool {
    a.get(..8)
        .zip(b.get(..8))
        .is_some_and(|(pa, pb)| pa.eq_ignore_ascii_case(pb))
        && a.get(8..) == b.get(8..)
}

/// 纯函数：过滤 `skill://` 前缀且 `/SKILL.md` 结尾的资源（入口约定
/// `skill://<path>/SKILL.md` 即一个技能；同前缀附属资源不单独注册）。
///
/// legacy 兜底：非规范路径（server 未声明 Skills 扩展）。身份规则仍对齐
/// 规范——skill name 是 frontmatter 的 `name`，URI 最终段须与它一致。
pub(crate) fn select_skill_resources(resources: &[Resource]) -> Vec<Resource> {
    resources
        .iter()
        .filter(|r| is_skill_scheme(&r.uri) && r.uri.ends_with("/SKILL.md"))
        .cloned()
        .collect()
}

/// 纯函数：嵌套过滤。候选 A 是嵌套技能 ⇔ 存在另一候选 B，A 的 URI 位于
/// B 的 skill 目录（B.uri 去 `/SKILL.md` 后缀）之下。
///
/// 嵌套 SKILL.md 是外层技能的支持文件（规范），其 frontmatter 不得生效，
/// 不单独注册——激活需 fresh consent，legacy 扫描无此机制，故直接过滤。
fn filter_nested_skills(resources: Vec<Resource>) -> Vec<Resource> {
    resources
        .iter()
        .filter(|r| {
            !resources.iter().any(|other| {
                other.uri != r.uri
                    && other
                        .uri
                        .strip_suffix("/SKILL.md")
                        .map(|dir| r.uri.starts_with(&format!("{dir}/")))
                        .unwrap_or(false)
            })
        })
        .cloned()
        .collect()
}

/// legacy 路径：并发读取候选资源（Semaphore(8) + JoinSet）：每条 read 返回
/// 后检查 cancel 提前退出；单条 30s 超时；解析/校验失败的条目静默过滤。
///
/// 返回 `(need_summary_warn, entries)`：嵌套过滤后无候选 → `(false, 空)`
/// （静默）；候选非空但全部失败 → `(true, 空)`（汇总 warn 由调用方在
/// cancel 检查之后发出，cancel 提前退出不得误报）。
pub(crate) async fn collect_skill_entries(
    peer: Peer<RoleClient>,
    server: &str,
    resources: Vec<Resource>,
    cancel: AgentCancellationToken,
) -> (bool, Vec<SkillMetadata>) {
    let resources = filter_nested_skills(resources);
    if resources.is_empty() {
        return (false, Vec::new());
    }
    let semaphore = Arc::new(tokio::sync::Semaphore::new(READ_CONCURRENCY));
    let server_owned = server.to_string();
    let mut join_set = tokio::task::JoinSet::new();
    let task_count = resources.len();

    for resource in resources {
        let permit_sem = Arc::clone(&semaphore);
        let task_peer = peer.clone();
        let task_server = server_owned.clone();
        let task_cancel = cancel.clone();
        join_set.spawn(async move {
            if task_cancel.is_cancelled() {
                return None;
            }
            let _permit = permit_sem.acquire_owned().await;
            let uri = resource.uri.clone();
            let request = ReadResourceRequestParams::new(uri.clone());
            let result = match tokio::time::timeout(RESOURCE_READ_TIMEOUT, task_peer.read_resource(request))
                .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(err)) => {
                    tracing::debug!(server = %task_server, %uri, "MCP skill 资源读取失败: {err}");
                    return None;
                }
                Err(_) => {
                    tracing::debug!(server = %task_server, %uri, "MCP skill 资源读取超时 ({}s)", RESOURCE_READ_TIMEOUT.as_secs());
                    return None;
                }
            };
            let text = result.contents.iter().find_map(|content| match content {
                ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
                _ => None,
            });
            text.and_then(|text| parse_mcp_skill_md(&text, &task_server, &uri))
        });
    }

    let mut entries = Vec::with_capacity(task_count);
    while let Some(joined) = join_set.join_next().await {
        if cancel.is_cancelled() {
            // 提前退出：cancel 后不再等剩余任务（缩短 Started 悬挂窗口）。
            join_set.abort_all();
            break;
        }
        if let Ok(Some(metadata)) = joined {
            entries.push(metadata);
        }
    }
    // JoinSet 完成序非确定：按 name 排序保证输出确定（同一 server 条目名互异，
    // 排序稳定；下游 registry 条目序/commands 列表随之确定）。
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let out = disambiguate_names(server, entries);
    (out.is_empty(), out)
}

// ─── 共享：frontmatter 解析 / 身份 / 校验 / 消歧 ──────────────────────────

/// 纯函数：gray_matter 解析 frontmatter 为 verbatim JSON map（loader.rs
/// 同款；YAML→JSON 值渲染由 serde_yaml 完成——数字 42 → Number、字符串
/// "42" → String）。YAML 非法 / 无 frontmatter → None。
fn parse_skill_frontmatter_map(
    content: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let matter = Matter::<YAML>::new();
    let result: gray_matter::ParsedEntity = matter.parse(content).ok()?;
    let data = result.data?;
    data.deserialize().ok()
}

/// 纯函数：frontmatter 逐字段全量比对（规范 MUST：host 拉取 SKILL.md 后须
/// 与条目 frontmatter 逐字段比对，**任何**差异——含附加字段如
/// license/metadata——验证失败、MUST NOT load）。键集合须完全一致，值经
/// [`frontmatter_values_equal`] 宽松归一（容忍 YAML→JSON 渲染差异）。
fn frontmatter_maps_equal(
    a: &serde_json::Map<String, serde_json::Value>,
    b: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|bv| frontmatter_values_equal(v, bv)))
}

/// 纯函数：frontmatter 值严格比较（规范 "identical in content" 字面）。
///
/// 决策（2026-08-15 第二轮 review 定案）：
/// - 数字 ↔ 字符串**跨类型不相等**（42 ≠ "42"）——两侧类型不一致即内容
///   差异，按规范拒绝；
/// - Number vs Number 保留 serde_json 混合 f64 比较（1 == 1.0 成立）；
/// - String vs String 做**尾随空白归一**（YAML block scalar 渲染差异：
///   比较前两侧 trim_end；不做 trim 全量——仅尾随）；
/// - 对象/数组逐字段递归；null vs 缺键由键集合长度区分。
fn frontmatter_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        (serde_json::Value::Bool(x), serde_json::Value::Bool(y)) => x == y,
        (serde_json::Value::String(x), serde_json::Value::String(y)) => {
            x.trim_end() == y.trim_end()
        }
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => number_eq(x, y),
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(xv, yv)| frontmatter_values_equal(xv, yv))
        }
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            frontmatter_maps_equal(x, y)
        }
        // 其余跨类型（含 Number vs String）→ 不相等。
        _ => false,
    }
}

/// 纯函数：Number vs Number 混合比较——serde_json `Number` 的 `PartialEq`
/// 是严格同变体（`PosInt(1) != Float(1.0)`）；决策保留混合 f64 比较
/// （1 == 1.0 成立）：先按整数精确比对，再走 Float-vs-Int 精确分支，最后
/// 退回纯浮点比较。
fn number_eq(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x == y;
    }
    // Float-vs-Int 精确分支（2026-08-15 第三轮 review）：f64 兜底对
    // >2^53 的 Float-vs-Int 会因 f64 舍入误判相等（如 2^53+1 Float 舍入后
    // 与 2^53 Int 的 f64 表示相同）。Float 绝对值 ≤ 2^53 时 f64 可精确表示
    // 该整数，转 i64/u64 精确比较；超出 f64 精确整数域 → 保守判不等
    // （拒绝）。该分支是 Float-vs-Int 的**最终判定**，不再回退 f64 兜底
    // （兜底正是舍入误判来源）。
    if a.is_f64() != b.is_f64() {
        let (f, int) = if a.is_f64() {
            (a.as_f64().unwrap(), b)
        } else {
            (b.as_f64().unwrap(), a)
        };
        return float_vs_int(f, int);
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 纯函数：Float 侧与 Int 侧的精确比较（`number_eq` 的 Float-vs-Int 分支）。
/// Float 非有限 / 非整数 / 绝对值 > 2^53（超出 f64 精确整数域）→ false
/// （保守拒绝）。
fn float_vs_int(f: f64, int: &serde_json::Number) -> bool {
    const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0; // 2^53
    if !f.is_finite() || f.abs() > MAX_EXACT_INT || f.fract() != 0.0 {
        return false;
    }
    if let Some(i) = int.as_i64() {
        return f as i64 == i;
    }
    if let Some(u) = int.as_u64() {
        return f as u64 == u;
    }
    false
}

/// 纯函数：`skill://` URI 路径段（scheme 大小写不敏感 + `strip_suffix(
/// "/SKILL.md")`，按 `/` 拆分；至少 1 段）。非 `skill://` scheme 或结构
/// 非法 → None。
fn uri_skill_segments(uri: &str) -> Option<Vec<String>> {
    if !is_skill_scheme(uri) {
        return None;
    }
    let path = uri.get(8..)?.strip_suffix("/SKILL.md")?;
    if path.is_empty() {
        return None;
    }
    Some(path.split('/').map(String::from).collect())
}

/// 纯函数：URI 最终段（= skill name，规范：最终段 MUST 等于 frontmatter name）。
fn uri_final_segment(uri: &str) -> Option<String> {
    uri_skill_segments(uri).and_then(|segments| segments.last().cloned())
}

/// 纯函数：非 `[a-zA-Z0-9_-]` 字符替换为 `'_'`（Agent Skills name 命名规则
/// 的宽松近似，用于与 URI 段做等价比对）。
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 纯函数：sha256 digest 校验（格式条文为 `sha256:{64 位小写 hex}`）。
/// **接受大写 hex 为互操作宽容**——server 生成大写 hex digest 是合法场景，
/// 校验前统一转小写比较（`is_ascii_hexdigit` 已覆盖大小写，非 hex 字符拒绝）。
/// 内容侧统一以 bytes 计算：Text 用 UTF-8 bytes、Blob 用 base64 解码后的
/// raw bytes——与 server 计算 digest 的字节一致。发现侧（skill_discovery）
/// 与读取面（resource_tool 完整性校验）共用。
pub(crate) fn verify_digest_bytes(content: &[u8], expected: &str) -> bool {
    let Some(hex) = expected.strip_prefix("sha256:") else {
        return false;
    };
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(content);
    let actual = hasher.finalize();
    let actual_hex: String = actual.iter().map(|b| format!("{b:02x}")).collect();
    actual_hex == hex.to_ascii_lowercase()
}

/// 纯函数：文本内容 digest 校验（`verify_digest_bytes` 的 UTF-8 便捷面）。
pub(crate) fn verify_digest(content: &str, expected: &str) -> bool {
    verify_digest_bytes(content.as_bytes(), expected)
}

/// 纯函数：身份校验 + 构建 metadata。
///
/// 注册名 = `mcp__<server>__<name>`（name = frontmatter `name`，经与 URI
/// 最终段一致性校验——frontmatter 是不可信内容，提示注入防御）；URI 最终段
/// 与 name（sanitize 后）不一致 → 拒绝。origin 保留完整 SKILL.md URI；
/// resources 存条目完整资源清单（读取面按它做内容绑定校验）。
fn build_metadata(
    server: &str,
    uri: &str,
    name: &str,
    description: &str,
    content: &str,
    resources: Vec<SkillResource>,
) -> Option<SkillMetadata> {
    let final_segment = uri_final_segment(uri)?;
    if sanitize_name(&final_segment) != sanitize_name(name) {
        tracing::warn!(
            server,
            %uri,
            "MCP skill uri 最终段与 frontmatter name 不一致，拒绝加载"
        );
        return None;
    }
    Some(SkillMetadata {
        name: mcp_skill_name(server, &sanitize_name(name)),
        description: description.trim().to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: uri.to_string(),
        }),
        content: Some(content.to_string()),
        // MCP 来源：完整 resources 集（读取面按它做内容绑定校验）
        resources,
    })
}

/// legacy 入口：frontmatter 解析（name/description 必填）+ 身份校验 +
/// 构建（兼容既有测试/调用面）。legacy 无 resources 清单 → 空 vec。
pub(crate) fn parse_mcp_skill_md(content: &str, server: &str, uri: &str) -> Option<SkillMetadata> {
    let fm = parse_skill_frontmatter_map(content)?;
    let name = fm.get("name")?.as_str()?;
    let description = fm.get("description")?.as_str()?;
    build_metadata(server, uri, name, description, content, vec![])
}

/// 纯函数：同一 server 内注册名冲突消歧。同名组 >1 时组内全部改用完整路径
/// 段形式（`mcp__<server>__<sanitized path segments>`）——规范：MUST
/// disambiguate（如按可区分路径段），不得静默丢弃或偏爱其一。非 `skill://`
/// scheme 无路径段可用 → 保留原名（host 不得假定名称唯一，按规范允许）。
fn disambiguate_names(server: &str, mut entries: Vec<SkillMetadata>) -> Vec<SkillMetadata> {
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, entry) in entries.iter().enumerate() {
        groups.entry(entry.name.clone()).or_default().push(i);
    }
    for (_, indices) in groups {
        if indices.len() < 2 {
            continue;
        }
        for i in indices {
            let path_name = entries[i]
                .origin
                .as_ref()
                .and_then(|origin| match origin {
                    SkillOrigin::Mcp { uri, .. } => uri_skill_segments(uri),
                })
                .map(|segments| {
                    segments
                        .iter()
                        .map(|s| sanitize_name(s))
                        .collect::<Vec<_>>()
                        .join("_")
                });
            match path_name {
                Some(path) => entries[i].name = mcp_skill_name(server, &path),
                None => tracing::warn!(
                    server,
                    skill = %entries[i].name,
                    "MCP skill 同名冲突且 uri 无路径段可消歧（名称不保证唯一）"
                ),
            }
        }
    }
    entries
}

#[cfg(test)]
#[path = "skill_discovery_test.rs"]
mod tests;
