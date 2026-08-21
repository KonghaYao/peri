// ─── legacy 兼容路径：resources 扫描（Claude Code 早期生态）───────────────

use std::sync::Arc;

use peri_acp_types::skills::SkillMetadata;
use rmcp::{
    model::{ReadResourceRequestParams, Resource, ResourceContents},
    Peer, RoleClient,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::super::resource_cache::McpResourceCache;
use super::verify::disambiguate_names;
use super::{parse_mcp_skill_md, READ_CONCURRENCY, RESOURCE_READ_TIMEOUT};

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
pub(super) fn filter_nested_skills(resources: Vec<Resource>) -> Vec<Resource> {
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
    cache: Option<(McpResourceCache, String)>,
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
        let task_cache = cache.clone();
        join_set.spawn(async move {
            if task_cancel.is_cancelled() {
                return None;
            }
            let _permit = permit_sem.acquire_owned().await;
            let uri = resource.uri.clone();
            let cached = match task_cache.as_ref() {
                Some((cache, origin)) => cache
                    .get(origin, "skills/legacy-read", &uri)
                    .await,
                None => None,
            };
            let result = if let Some(result) = cached {
                result
            } else {
                if let Some((cache, origin)) = task_cache.as_ref() {
                    cache.mark_live_fetch(origin, "skills/legacy-read");
                }
                let ticket = match task_cache.as_ref() {
                    Some((cache, origin)) => {
                        cache.ticket(origin, "skills/legacy-read", &uri).await
                    }
                    None => None,
                };
                let request = ReadResourceRequestParams::new(uri.clone());
                let result = match tokio::time::timeout(
                    RESOURCE_READ_TIMEOUT,
                    task_peer.read_resource(request),
                )
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
                if let (Some((cache, _)), Some(ticket)) = (task_cache.as_ref(), ticket) {
                    if result.cache_scope == Some(rmcp::model::CacheScope::Public) {
                        cache
                            .put_ticket(
                                &ticket,
                                std::time::Duration::from_millis(
                                    result.ttl_ms.unwrap_or_default(),
                                ),
                                &result,
                            )
                            .await;
                    }
                }
                result
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
