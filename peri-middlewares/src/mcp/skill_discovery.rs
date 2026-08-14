//! MCP `skill://` 资源异步发现：候选过滤 → 并发 `resources/read` 读全文 →
//! frontmatter 解析 → 写 session 级 `McpSkillRegistry`。
//!
//! 异步语义（spec B.1）：不阻塞连接初始化与首 turn；发现完成静默（不注入
//! agent 上下文）；失败仅告警日志，不影响连接。任务由 McpMiddleware 的
//! `before_agent` 投影 spawn，持 session cancel token。

use std::{path::PathBuf, sync::Arc};

use gray_matter::{engine::YAML, Matter};
use peri_acp_types::{
    mcp_skills::{mcp_skill_name, HandleToken, McpSkillRegistry},
    skills::{SkillMetadata, SkillOrigin, SkillSource},
};
use rmcp::{
    model::{ReadResourceRequestParams, Resource, ResourceContents},
    Peer, RoleClient,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::client::McpClientHandle;

/// 单条资源读取超时（cancel 后悬挂窗口上界 = (N/8)×30s）。
const RESOURCE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 并发读取上限（tokio 现成原语，无新依赖）。
const READ_CONCURRENCY: usize = 8;

/// 发现任务主体：候选过滤 → 并发读全文 → 解析 → 回写 registry。
///
/// - candidates 空 → 直接完成（空条目）；
/// - peer 缺失 → warn + 完成（失败=空条目，不重试；重连才重扫）；
/// - cancel 触发 → 回退 Started 状态（不触发 on_change），下轮可重试。
pub(crate) async fn run_discovery(
    registry: Arc<McpSkillRegistry>,
    handle: Arc<McpClientHandle>,
    handle_token: HandleToken,
    cancel: AgentCancellationToken,
) {
    let candidates = select_skill_resources(&handle.resources);
    if candidates.is_empty() {
        registry.mark_discovery_completed(&handle.name, handle_token, vec![]);
        return;
    }
    let candidate_count = candidates.len();
    let Some(peer) = handle.peer.clone() else {
        tracing::warn!(server = %handle.name, "MCP skill 发现：peer 缺失，跳过");
        registry.mark_discovery_completed(&handle.name, handle_token, vec![]);
        return;
    };
    let entries = collect_skill_entries(peer, &handle.name, candidates, cancel.clone()).await;
    if cancel.is_cancelled() {
        registry.clear_discovery_started(&handle.name, handle_token);
        return;
    }
    if entries.is_empty() {
        // 候选非空但全部读取/解析失败：告警可见（仅 server 名与条数，无内容/secret）。
        tracing::warn!(
            server = %handle.name,
            count = candidate_count,
            "MCP skill 发现：{} 条 skill 资源全部读取失败，无可用条目",
            candidate_count,
        );
    }
    registry.mark_discovery_completed(&handle.name, handle_token, entries);
}

/// 并发读取候选资源（Semaphore(8) + JoinSet）：每条 read 返回后检查 cancel
/// 提前退出；单条 30s 超时；解析失败的条目静默过滤。
///
/// peer 为 Clone（`rmcp::Peer` 内部是 mpsc Sender + Arc），每条任务持 clone。
pub(crate) async fn collect_skill_entries(
    peer: Peer<RoleClient>,
    server: &str,
    resources: Vec<Resource>,
    cancel: AgentCancellationToken,
) -> Vec<SkillMetadata> {
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
    entries
}

/// 纯函数：过滤 `skill://` 前缀且 `/SKILL.md` 结尾的资源（入口约定
/// `skill://<name>/SKILL.md` 即一个技能；同前缀附属资源不单独注册）。
pub(crate) fn select_skill_resources(resources: &[Resource]) -> Vec<Resource> {
    resources
        .iter()
        .filter(|r| r.uri.starts_with("skill://") && r.uri.ends_with("/SKILL.md"))
        .cloned()
        .collect()
}

/// frontmatter 反序列化结构（name 仅存在性校验，不写入 metadata）。
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    /// 仅作存在性校验门（serde 反序列化强制存在），不写入 metadata——
    /// 身份取自 uri 段而非不可信的 frontmatter。
    #[allow(dead_code)]
    name: String,
    description: String,
}

/// 纯函数：gray_matter 解析 frontmatter（loader.rs 同款）；name/description
/// 任一缺失 → None。
///
/// 注册名 = `mcp__<server>__<uri段>`（uri 段 = uri 去 `"skill://"` 前缀与
/// `"/SKILL.md"` 后缀，非 `[a-zA-Z0-9_-]` 字符替换为 `'_'`）——身份取自 uri
/// 而非 frontmatter（frontmatter 是不可信内容，提示注入防御）。
pub(crate) fn parse_mcp_skill_md(content: &str, server: &str, uri: &str) -> Option<SkillMetadata> {
    let matter = Matter::<YAML>::new();
    let result: gray_matter::ParsedEntity = matter.parse(content).ok()?;
    let data = result.data?;
    let fm: SkillFrontmatter = data.deserialize().ok()?;

    let segment = uri.strip_prefix("skill://")?.strip_suffix("/SKILL.md")?;
    let sanitized: String = segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    Some(SkillMetadata {
        name: mcp_skill_name(server, &sanitized),
        description: fm.description.trim().to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: server.to_string(),
            uri: uri.to_string(),
        }),
        content: Some(content.to_string()),
    })
}

#[cfg(test)]
#[path = "skill_discovery_test.rs"]
mod tests;
