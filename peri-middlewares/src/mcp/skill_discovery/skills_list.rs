// ─── SEP-2640 规范路径：skills/list ───────────────────────────────────────

use std::sync::Arc;

use super::super::resource_cache::McpResourceCache;
use peri_acp_types::skills::{SkillMetadata, SkillResource};
use rmcp::{
    model::{
        ClientRequest, CustomRequest, ReadResourceRequestParams, ResourceContents, ServerResult,
    },
    Peer, RoleClient,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::legacy_scan::uri_eq_ignore_scheme_case;
use super::verify::{
    build_metadata, disambiguate_names, frontmatter_maps_equal, parse_skill_frontmatter_map,
    verify_digest,
};
use super::{MAX_LIST_PAGES, READ_CONCURRENCY, RESOURCE_READ_TIMEOUT, SKILLS_LIST_TIMEOUT};

/// `skills/list` 响应（分页；`nextCursor` 缺省兼容未分页 server）。
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SkillListResponse {
    #[serde(default)]
    pub(super) skills: Vec<SkillListEntryDto>,
    #[serde(default)]
    pub(super) next_cursor: Option<String>,
}

/// `skills/list` 条目（`frontmatter` 为 SKILL.md YAML frontmatter 的
/// verbatim JSON 渲染——规范要求原样透传，非精选子集）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SkillListEntryDto {
    uri: String,
    frontmatter: serde_json::Map<String, serde_json::Value>,
    /// 完整资源清单 {uri, digest}；动态生成技能可省略（规范 MAY）。用
    /// `Option` 区分省略（None = 动态技能）与显式空数组/部分清单
    /// （Some——present 时必须完整，含 SKILL.md 自身条目）。
    #[serde(default)]
    resources: Option<Vec<SkillResourceDto>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
pub(super) struct SkillListEntry {
    pub(super) uri: String,
    /// SKILL.md YAML frontmatter 的 verbatim JSON 渲染（非精选子集）。
    /// `verify_and_build` 以此与读到的 frontmatter 全等比对。
    pub(super) frontmatter: serde_json::Map<String, serde_json::Value>,
    /// 完整资源清单；`None` = 省略（动态生成技能，规范 MAY）——接受但
    /// 无法内容绑定；`Some(..)` = 显式声明（present 时必须完整，含
    /// SKILL.md 自身条目，否则完整性违规拒绝）。
    pub(super) resources: Option<Vec<SkillResource>>,
}

/// 纯函数：DTO → 条目。frontmatter 缺 name/description（Agent Skills
/// 规范要求必填，缺失即非规范）→ None（该条目跳过）；frontmatter map
/// 原样移入（verbatim，不 clone 消耗）。
pub(super) fn entry_from_dto(dto: SkillListEntryDto) -> Option<SkillListEntry> {
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
pub(super) async fn collect_via_skills_list(
    peer: Peer<RoleClient>,
    server: &str,
    cancel: AgentCancellationToken,
) -> (bool, Vec<SkillMetadata>) {
    collect_via_skills_list_inner(peer, server, cancel, None).await
}

pub(super) async fn collect_via_skills_list_cached(
    peer: Peer<RoleClient>,
    server: &str,
    cancel: AgentCancellationToken,
    cache: McpResourceCache,
    origin: String,
) -> (bool, Vec<SkillMetadata>) {
    collect_via_skills_list_inner(peer, server, cancel, Some((cache, origin))).await
}

async fn collect_via_skills_list_inner(
    peer: Peer<RoleClient>,
    server: &str,
    cancel: AgentCancellationToken,
    cache_context: Option<(McpResourceCache, String)>,
) -> (bool, Vec<SkillMetadata>) {
    let mut dto_entries: Vec<SkillListEntryDto> = Vec::new();
    let mut cursor: Option<String> = None;
    for _page in 0..MAX_LIST_PAGES {
        if cancel.is_cancelled() {
            return (false, Vec::new());
        }
        let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let page = if let Some((cache, origin)) = cache_context.as_ref() {
            if let Some(page) = cache.get_json(origin, "skills/list", &params_key).await {
                page
            } else {
                // 必须在 RPC 前捕获 ticket：更新通知若在请求期间到达，旧分页
                // 响应随后不得重新写入持久化缓存。
                let ticket = cache.ticket(origin, "skills/list", &params_key).await;
                let page = fetch_skill_list_page(&peer, server, params).await;
                let Some(page) = page else {
                    return (false, Vec::new());
                };
                if let Some(ticket) = ticket {
                    cache
                        .put_ticket(&ticket, std::time::Duration::from_secs(300), &page)
                        .await;
                }
                page
            }
        } else {
            let Some(page) = fetch_skill_list_page(&peer, server, params).await else {
                return (false, Vec::new());
            };
            page
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
    let out = fetch_and_verify(peer, server, entries, cancel, cache_context).await;
    (out.is_empty(), out)
}

async fn fetch_skill_list_page(
    peer: &Peer<RoleClient>,
    server: &str,
    params: Option<serde_json::Value>,
) -> Option<SkillListResponse> {
    let request = ClientRequest::CustomRequest(CustomRequest::new("skills/list", params));
    let response = tokio::time::timeout(SKILLS_LIST_TIMEOUT, peer.send_request(request)).await;
    match response {
        Ok(Ok(ServerResult::CustomResult(custom))) => match custom.result_as() {
            Ok(page) => Some(page),
            Err(err) => {
                tracing::warn!(server, error = %err, "MCP skill 发现：skills/list 响应解析失败");
                None
            }
        },
        Ok(Ok(_)) => {
            tracing::warn!(server, "MCP skill 发现：skills/list 返回非预期响应类型");
            None
        }
        Ok(Err(err)) => {
            tracing::warn!(server, error = %err, "MCP skill 发现：skills/list 调用失败");
            None
        }
        Err(_) => {
            tracing::warn!(
                server,
                "MCP skill 发现：skills/list 超时 ({}s)",
                SKILLS_LIST_TIMEOUT.as_secs()
            );
            None
        }
    }
}

/// 并发读取并校验条目（Semaphore + JoinSet）：每条任务持 peer clone；
/// digest 校验 / frontmatter 比对 / URI 最终段校验任一失败 → 条目过滤。
async fn fetch_and_verify(
    peer: Peer<RoleClient>,
    server: &str,
    entries: Vec<SkillListEntry>,
    cancel: AgentCancellationToken,
    cache_context: Option<(McpResourceCache, String)>,
) -> Vec<SkillMetadata> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(READ_CONCURRENCY));
    let server_owned = server.to_string();
    let mut join_set = tokio::task::JoinSet::new();
    for entry in entries {
        let permit = Arc::clone(&semaphore);
        let task_peer = peer.clone();
        let task_server = server_owned.clone();
        let task_cancel = cancel.clone();
        let task_cache = cache_context.clone();
        join_set.spawn(async move {
            if task_cancel.is_cancelled() {
                return None;
            }
            let _permit = permit.acquire_owned().await;
            fetch_and_verify_one(task_peer, &task_server, entry, task_cancel, task_cache).await
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
    cache_context: Option<(McpResourceCache, String)>,
) -> Option<SkillMetadata> {
    if cancel.is_cancelled() {
        return None;
    }
    let uri = entry.uri.clone();
    // 发现侧首读：SKILL.md 必须是 Text（Blob/失败/超时均无法校验 frontmatter
    // 与 digest）→ 条目过滤。
    let (text, cache_ticket) = if let Some((cache, origin)) = cache_context.as_ref() {
        if let Some(text) = cache.get(origin, "skills/read", &uri).await {
            (text, None)
        } else {
            let ticket = cache.ticket(origin, "skills/read", &uri).await;
            let SkillResourceRead::Text(text, _) =
                read_skill_resource_text(&peer, server, &uri).await
            else {
                return None;
            };
            (text, ticket.map(|ticket| (cache.clone(), ticket)))
        }
    } else {
        let SkillResourceRead::Text(text, _) = read_skill_resource_text(&peer, server, &uri).await
        else {
            return None;
        };
        (text, None)
    };
    match verify_and_build(server, &entry, &text) {
        VerifyOutcome::Built(meta) => {
            if let Some((cache, ticket)) = cache_ticket {
                cache
                    .put_ticket(&ticket, std::time::Duration::from_secs(300), &text)
                    .await;
            }
            Some(*meta)
        }
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
        VerifyOutcome::Built(meta) => Some(*meta),
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
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => Some(SkillResourceRead::Text(text.clone(), mime_type.clone())),
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
pub(super) enum VerifyOutcome {
    /// 校验通过，metadata 构建成功
    Built(Box<SkillMetadata>),
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
pub(super) fn verify_and_build(
    server: &str,
    entry: &SkillListEntry,
    content: &str,
) -> VerifyOutcome {
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
        Some(meta) => VerifyOutcome::Built(Box::new(meta)),
        None => VerifyOutcome::Rejected,
    }
}
