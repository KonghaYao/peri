// ─── SEP-2640 规范路径：skills/list ───────────────────────────────────────

use super::super::client::cache_scope_allows_persistence;
use super::super::resource_cache::McpResourceCache;
use peri_acp_types::skills::{SkillMetadata, SkillResource};
use rmcp::{
    model::{
        CacheScope, ClientRequest, CustomRequest, ReadResourceRequestParams, ResourceContents,
        ServerResult,
    },
    Peer, RoleClient,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::legacy_scan::uri_eq_ignore_scheme_case;
use super::verify::{build_metadata, disambiguate_names, frontmatter_maps_equal, verify_digest};
use super::{MAX_LIST_PAGES, RESOURCE_READ_TIMEOUT, SKILLS_LIST_TIMEOUT};

/// `skills/list` 响应（分页；`nextCursor` 缺省兼容未分页 server）。
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SkillListResponse {
    #[serde(default)]
    pub(super) skills: Vec<SkillListEntryDto>,
    #[serde(default)]
    pub(super) next_cursor: Option<String>,
    #[serde(default)]
    pub(super) ttl_ms: Option<u64>,
    #[serde(default)]
    pub(super) cache_scope: Option<CacheScope>,
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
    /// activation 以此与读到的正文 frontmatter 全等比对。
    pub(super) frontmatter: serde_json::Map<String, serde_json::Value>,
    /// 完整资源清单；`None` = 省略（动态生成技能，规范 MAY）——接受但
    /// 无法内容绑定；`Some(..)` = 显式声明（present 时必须完整，含
    /// SKILL.md 自身条目，否则完整性违规拒绝）。
    pub(super) resources: Option<Vec<SkillResource>>,
}

/// 条目级结构缺陷（诊断用）：只带来源、字段与错误类别，**不含原始字段正文**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EntryDefect {
    /// 出错字段（frontmatter 字段名）。
    pub(super) field: &'static str,
    /// 错误类别（`missing` / `wrong-type`）。
    pub(super) category: &'static str,
}

impl EntryDefect {
    fn missing(field: &'static str) -> Self {
        Self {
            field,
            category: "missing",
        }
    }

    fn wrong_type(field: &'static str) -> Self {
        Self {
            field,
            category: "wrong-type",
        }
    }
}

/// 纯函数：DTO → 条目。frontmatter `name` / `description` 必填且必须是字符串
/// （Agent Skills 规范要求；缺失或类型不符即非规范条目，M8：隔离该条目并给出
/// 字段与类别诊断，不回显字段正文）；frontmatter map 原样移入（verbatim，
/// 不 clone 消耗）。
pub(super) fn entry_from_dto(dto: SkillListEntryDto) -> Result<SkillListEntry, EntryDefect> {
    for field in ["name", "description"] {
        match dto.frontmatter.get(field) {
            None => return Err(EntryDefect::missing(field)),
            Some(value) if value.as_str().is_none() => return Err(EntryDefect::wrong_type(field)),
            Some(_) => {}
        }
    }
    Ok(SkillListEntry {
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

/// 逐条转换并**隔离**非法条目：坏条目单独记来源 / 字段 / 错误类别（不回显字段
/// 正文），好条目照常进入发现结果；有隔离时另记汇总 warn——部分失败不得被
/// 呈现成完整成功（M8）。返回 `(entries, rejected)`。
pub(super) fn collect_entries(
    server: &str,
    dto_entries: Vec<SkillListEntryDto>,
    context: &str,
) -> (Vec<SkillListEntry>, usize) {
    let total = dto_entries.len();
    let mut entries = Vec::with_capacity(total);
    let mut rejected = 0usize;
    for dto in dto_entries {
        let uri = dto.uri.clone();
        match entry_from_dto(dto) {
            Ok(entry) => entries.push(entry),
            Err(defect) => {
                rejected += 1;
                tracing::warn!(
                    server,
                    uri = %uri,
                    field = defect.field,
                    category = defect.category,
                    "MCP skill 发现：条目字段非法，隔离该条目（不回显字段正文）"
                );
            }
        }
    }
    if rejected > 0 {
        tracing::warn!(
            server,
            context,
            rejected,
            total,
            "MCP skill 发现：部分条目被隔离，本次结果不是完整成功"
        );
    }
    (entries, rejected)
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
                cache.mark_live_fetch(origin, "skills/list");
                let ticket = cache.ticket(origin, "skills/list", &params_key).await;
                let page = fetch_skill_list_page(&peer, server, params).await;
                let Some(page) = page else {
                    return (false, Vec::new());
                };
                if cache_scope_allows_persistence(page.cache_scope)
                    || (page.cache_scope.is_none() && page.ttl_ms.is_some())
                {
                    if let Some(ticket) = ticket {
                        cache
                            .put_ticket(
                                &ticket,
                                std::time::Duration::from_millis(page.ttl_ms.unwrap_or_default()),
                                &page,
                            )
                            .await;
                    }
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
    let (entries, _rejected) = collect_entries(server, dto_entries, "skills/list 发现");
    if entries.is_empty() {
        tracing::warn!(
            server,
            "MCP skill 发现：skills/list 条目全部缺 frontmatter name/description"
        );
        return (false, Vec::new());
    }
    // W2：发现只发布 metadata——**不读正文**（正文与完整性校验归统一 activation）。
    entries_to_metadata(server, entries)
}

/// 保留错误的 `skills/list` 单页读取（F3 冻结快照用）。
///
/// 与 [`fetch_skill_list_page`] 是**同一实现**：发现路径把错误吞成
/// `None`（best-effort，不进首轮阻塞），而冻结期快照按 X5 需要区分「空技能集」
/// 与「读取失败」（后者 fail-closed），因此错误在这里以 `Err(String)` 上抛，
/// 文案不含主机路径与正文。
async fn fetch_skill_list_page_checked(
    peer: &Peer<RoleClient>,
    server: &str,
    params: Option<serde_json::Value>,
) -> Result<SkillListResponse, String> {
    fetch_skill_list_page_inner(peer, server, params).await
}

/// 单页读取（供 checked 与 best-effort 两条路径共用）。
async fn fetch_skill_list_page_inner(
    peer: &Peer<RoleClient>,
    server: &str,
    params: Option<serde_json::Value>,
) -> Result<SkillListResponse, String> {
    let request = ClientRequest::CustomRequest(CustomRequest::new("skills/list", params));
    let response = peri_time::timeout(SKILLS_LIST_TIMEOUT, peer.send_request(request)).await;
    match response {
        Ok(Ok(ServerResult::CustomResult(custom))) => custom.result_as().map_err(|err| {
            tracing::warn!(server, error = %err, "MCP skill 发现：skills/list 响应解析失败");
            format!("skills/list 响应解析失败: {err}")
        }),
        Ok(Ok(_)) => {
            tracing::warn!(server, "MCP skill 发现：skills/list 返回非预期响应类型");
            Err("skills/list 返回非预期响应类型".to_string())
        }
        Ok(Err(err)) => {
            tracing::warn!(server, error = %err, "MCP skill 发现：skills/list 调用失败");
            Err(format!("skills/list 调用失败: {err}"))
        }
        Err(_) => {
            tracing::warn!(
                server,
                "MCP skill 发现：skills/list 超时 ({}s)",
                SKILLS_LIST_TIMEOUT.as_secs()
            );
            Err(format!(
                "skills/list 超时 ({}s)",
                SKILLS_LIST_TIMEOUT.as_secs()
            ))
        }
    }
}

/// 发现路径的单页读取（best-effort：错误记日志后吞成 `None`，既有语义不变）。
async fn fetch_skill_list_page(
    peer: &Peer<RoleClient>,
    server: &str,
    params: Option<serde_json::Value>,
) -> Option<SkillListResponse> {
    fetch_skill_list_page_inner(peer, server, params).await.ok()
}

/// 冻结期技能清单快照（F3）：分页读完 `skills/list`，**不读正文**、不写注册表、
/// 不落任何缓存，错误原样上抛（调用方按 X5 决定 fail-closed 或降级）。
///
/// 与 [`collect_via_skills_list`] 的差别只有失败语义：发现是 best-effort
/// （空结果 + 日志），冻结快照必须能区分「技能集为空」（正常）与「读取失败」
/// （system 已声明且被选中的投递失败 ⇒ fail-closed）。
pub(crate) async fn snapshot_via_skills_list(
    peer: Peer<RoleClient>,
    server: &str,
    cancel: AgentCancellationToken,
) -> Result<Vec<SkillMetadata>, String> {
    let mut dto_entries: Vec<SkillListEntryDto> = Vec::new();
    let mut cursor: Option<String> = None;
    for _page in 0..MAX_LIST_PAGES {
        if cancel.is_cancelled() {
            return Err("skills/list 读取被取消".to_string());
        }
        let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
        let page = fetch_skill_list_page_checked(&peer, server, params).await?;
        dto_entries.extend(page.skills);
        let next = page.next_cursor;
        if next.as_ref().is_some() && next.as_ref() == cursor.as_ref() {
            tracing::warn!(server, "MCP skill 快照：skills/list 游标不前进，终止分页");
            break;
        }
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    if dto_entries.is_empty() {
        return Ok(Vec::new());
    }
    let (entries, _rejected) = collect_entries(server, dto_entries, "skills/list 冻结快照");
    if entries.is_empty() {
        tracing::warn!(
            server,
            "MCP skill 快照：skills/list 条目全部缺 frontmatter name/description"
        );
        return Ok(Vec::new());
    }
    Ok(entries_to_metadata(server, entries).1)
}

/// 共享恢复流程：`skills/get` 拉取当前条目快照（W2：**不读正文**）。
///
/// 响应 uri 与请求核对（scheme 大小写不敏感；不一致 = server 违规，拒绝恢复）
/// → 按新条目做结构校验（身份 / manifest 完整性）。内容完整性由调用方按新条目
/// 快照自行读取校验（activation 与读取面各自持请求上下文）。
pub(crate) async fn recover_entry_via_skills_get(
    peer: &Peer<RoleClient>,
    server: &str,
    uri: &str,
) -> Option<SkillMetadata> {
    let refreshed = fetch_skill_entry(peer, uri).await?;
    if !uri_eq_ignore_scheme_case(&refreshed.uri, uri) {
        tracing::warn!(
            server,
            %uri,
            "MCP skill skills/get 返回 uri 与请求不一致，拒绝恢复"
        );
        return None;
    }
    match entry_to_metadata(server, &refreshed) {
        Some(meta) => {
            tracing::debug!(server, %uri, "MCP skill 条目已按 skills/get 刷新");
            Some(meta)
        }
        None => {
            tracing::warn!(
                server,
                %uri,
                "MCP skill skills/get 恢复失败：新条目结构非法，拒绝加载"
            );
            None
        }
    }
}

/// 读取面热更新恢复（resource_tool 专用，pub(crate)）：digest 不匹配 /
/// 未列出时经 `skills/get` 刷新单条目，并按**新条目**校验请求 uri 内容。
///
/// 1. `skills/get` 拉取覆盖条目（entry_uri）的当前快照（响应 uri 核对，
///    scheme 大小写不敏感）+ 结构校验；
/// 2. 在**新条目**的 resources 中定位请求 uri：未列出 → 拒绝；
/// 3. 读取请求 uri 并按该 uri 的 digest 校验；请求 uri 是 SKILL.md 自身时
///    额外做 frontmatter 全量比对（与 activation 同一规则）。
///
/// 成功 → `Some((新条目 metadata, 请求 uri 内容, mime))`（调用方回写 registry）；
/// 任一失败 → `None`。恢复只尝试一次；get/read 各自 30s 超时。
pub(crate) async fn refresh_entry_and_content(
    peer: &Peer<RoleClient>,
    server: &str,
    entry_uri: &str,
    request_uri: &str,
) -> Option<(SkillMetadata, String, Option<String>)> {
    let meta = recover_entry_via_skills_get(peer, server, entry_uri).await?;
    // 附属资源与 SKILL.md 走同一路径：按新条目 resources 定位 digest。
    let Some(expected) = meta
        .resources
        .iter()
        .find(|resource| uri_eq_ignore_scheme_case(&resource.uri, request_uri))
    else {
        tracing::warn!(
            server,
            %request_uri,
            "MCP skill 热更新恢复失败：新条目未列出请求 uri，拒绝加载"
        );
        return None;
    };
    let (text, mime) = match read_skill_resource_text(peer, server, request_uri).await {
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
    // SKILL.md 自身：内容即身份载体，frontmatter 必须与刷新条目全量一致。
    if uri_eq_ignore_scheme_case(request_uri, entry_uri) {
        let Some(expected_fm) = meta.frontmatter.as_ref() else {
            tracing::warn!(server, %request_uri, "MCP skill 恢复失败：刷新条目缺 frontmatter 快照");
            return None;
        };
        let Some(actual_fm) = super::verify::parse_skill_frontmatter_map(&text) else {
            tracing::warn!(server, %request_uri, "MCP skill 恢复失败：正文 frontmatter 解析失败");
            return None;
        };
        if !frontmatter_maps_equal(&actual_fm, expected_fm) {
            tracing::warn!(
                server,
                %request_uri,
                "MCP skill 恢复失败：正文 frontmatter 与刷新条目不一致，拒绝加载"
            );
            return None;
        }
    }
    Some((meta, text, mime))
}

/// 单次 `resources/read` 的读取结果三态：成功 Text（携带文本与 mime）、
/// RPC 失败/超时、响应成功但无 Text 内容（Blob 资源）。区分后两者供恢复
/// 路径给出准确文案（NotText 不是传输层错误）。
///
/// W2：读取只发生在激活/读取面（发现期不读正文），响应缓存元数据不再消费，
/// 因此不携带 ttl / cacheScope。
pub(crate) enum SkillResourceRead {
    /// Text 内容（文本 + mime）
    Text(String, Option<String>),
    /// RPC 失败/超时
    Failed,
    /// 响应成功但无 Text 内容（Blob 资源——恢复路径仅支持 Text）
    NotText,
}

/// 单次 `resources/read`：读 SKILL.md 文本（30s 超时；失败/超时 →
/// [`SkillResourceRead::Failed`] + debug；响应无 Text → [`SkillResourceRead::NotText`]）。
pub(crate) async fn read_skill_resource_text(
    peer: &Peer<RoleClient>,
    server: &str,
    uri: &str,
) -> SkillResourceRead {
    let request = ReadResourceRequestParams::new(uri.to_string());
    let result = match peri_time::timeout(RESOURCE_READ_TIMEOUT, peer.read_resource(request)).await
    {
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
    let response = peri_time::timeout(SKILLS_LIST_TIMEOUT, peer.send_request(request)).await;
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
    match entry_from_dto(parsed.skill) {
        Ok(entry) => Some(entry),
        Err(defect) => {
            tracing::warn!(
                %uri,
                field = defect.field,
                category = defect.category,
                "MCP skill skills/get 条目字段非法，隔离该条目（不回显字段正文）"
            );
            None
        }
    }
}

/// 条目 → metadata（W2：**发现期不读正文**）。
///
/// 只做**结构**校验（不触碰内容字节）：
/// - frontmatter `name` / `description` 必填（`entry_from_dto` 已保证）；
/// - URI 最终段与 frontmatter `name` 一致（身份；frontmatter 是不可信内容）；
/// - 声明了 `resources` 时必须完整（含 SKILL.md 自身条目）——显式空数组 /
///   缺自身条目是完整性违规，条目拒绝；
/// - 省略 `resources`（动态生成技能，规范 MAY）→ 条目接受但**无内容绑定**：
///   激活按保守策略拒绝（X5/X6：不静默伪造完整性）。
///
/// 正文完整性（digest + frontmatter 全量比对）**不在发现期做**：统一 activation
/// 在激活时按本函数产出的 `resources[]` / `frontmatter` 快照校验。
pub(crate) fn entry_to_metadata(server: &str, entry: &SkillListEntry) -> Option<SkillMetadata> {
    match &entry.resources {
        Some(resources) => {
            if !resources.iter().any(|resource| resource.uri == entry.uri) {
                tracing::warn!(
                    server,
                    uri = %entry.uri,
                    "MCP skill resources 未含 SKILL.md 自身条目（完整性违规），拒绝加载"
                );
                return None;
            }
        }
        None => {
            tracing::debug!(
                server,
                uri = %entry.uri,
                "MCP skill 条目省略 resources（动态生成技能）：无内容绑定，激活按保守策略拒绝"
            );
        }
    }
    let name = entry.frontmatter.get("name")?.as_str()?;
    let description = entry.frontmatter.get("description")?.as_str()?;
    build_metadata(
        server,
        &entry.uri,
        name,
        description,
        None,
        entry.resources.clone().unwrap_or_default(),
        entry.frontmatter.clone(),
    )
}

/// 批量：条目 → metadata（结构非法的条目跳过），输出按注册名排序（完成序非
/// 确定，排序保证下游 registry 条目序 / commands 列表确定），随后做同 server
/// 名冲突消歧。返回 `(候选非空但全部被拒, 条目)`。
pub(crate) fn entries_to_metadata(
    server: &str,
    entries: Vec<SkillListEntry>,
) -> (bool, Vec<SkillMetadata>) {
    let total = entries.len();
    let mut out: Vec<SkillMetadata> = entries
        .iter()
        .filter_map(|entry| entry_to_metadata(server, entry))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    let all_rejected = total > 0 && out.is_empty();
    (all_rejected, disambiguate_names(server, out))
}
