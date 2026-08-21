use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use peri_acp_types::{
    mcp_skills::{HandleToken, McpSkillRegistry, ServerDiscoveryState},
    skills::{SkillMetadata, SkillOrigin},
};
use peri_agent::tools::BaseTool;
use thiserror::Error;

use super::client::{ClientStatus, McpClientPool};
use super::skill_discovery::{
    is_skill_scheme, refresh_entry_and_content, uri_eq_ignore_scheme_case, verify_digest,
    verify_digest_bytes,
};
use crate::tools::output_persist::persist_truncated_output;

/// 资源读取工具错误
#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("MCP 服务器 \"{server}\" 未找到")]
    ServerNotFound { server: String },
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected {
        server: String,
        status: ClientStatus,
    },
    #[error("MCP 资源读取失败: {server}: {reason}")]
    ReadFailed { server: String, reason: String },
    #[error("MCP 资源读取参数错误: {0}")]
    InvalidParam(String),
    /// skill:// 资源内容绑定校验失败（未列入条目 resources / digest 不匹配）。
    /// 语义对齐 SEP-2640：等同发现侧 digest 校验失败，MUST NOT use。
    #[error("MCP 资源完整性校验失败: {server}: {reason}")]
    VerificationFailed { server: String, reason: String },
}

const TOOL_NAME: &str = "mcp_read_resource";
const RESOURCE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_MCP_LINES: usize = 2000;

/// MCP 资源读取工具——统一资源读取入口
pub struct McpResourceTool {
    client_pool: Arc<McpClientPool>,
    /// session 级 MCP skill 远端注册表（读面完整性校验；空注册表 = 不校验）。
    /// 架构硬约束（issue C 节）：挂 session 装配链，绝不挂 pool/全局。
    registry: Arc<McpSkillRegistry>,
    cached_description: String,
}

impl McpResourceTool {
    pub fn new(client_pool: Arc<McpClientPool>, registry: Arc<McpSkillRegistry>) -> Self {
        let summary = client_pool.resource_summary();
        let cached_description = if summary.is_empty() {
            "Read a resource from an MCP server. No resources currently available.".to_string()
        } else {
            format!(
                "Read a resource from an MCP server. Available resources:\n{}",
                summary
            )
        };
        Self {
            client_pool,
            registry,
            cached_description,
        }
    }

    /// 读取面热更新恢复（digest 不匹配 / 未列出时，仅 skill:// 且条目覆盖）：
    /// `skills/get` 拉取当前条目快照 → 按新条目重读内容并全量校验 →
    /// `refresh_entries` 回写 registry。成功 → `Some((请求 uri 的新内容,
    /// mime))`；任一失败 → `None`（调用方保持 VerificationFailed）。
    ///
    /// - 恢复只尝试一次；get/read 各自 30s 超时（skill_discovery 内）：
    ///   Listed 分支上界约 210s（首读 120s + 恢复 ≤90s），Unlisted 分支
    ///   ≤90s；工具无整体超时（timeout()=None），悬挂受 agent 层控制；
    /// - **读取面无 cancel 机制**：恢复流程（get + 重读，最长 ≤90s）不可
    ///   中断，仅外层（首读/调用方）可取消；
    /// - handle 快照窗口：invoke 开头取 entry 快照，经 120s 首读窗口后
    ///   recover 才取当前 handle——窗口内重连的新 handle 会接受回写（防的
    ///   是恢复 RPC 期间（get/read 30s×N）的竞态，防不了 invoke→recover
    ///   窗口）；回写还可能覆盖发现任务刚写入的新条目（无版本戳，已知
    ///   限制）；内容已全量校验，仍返回（避免 agent 反复读同一陈旧内容）；
    /// - Started 状态（发现任务进行中）不恢复——整体覆盖以发现完成为准。
    async fn recover_via_refresh(
        &self,
        peer: &rmcp::Peer<rmcp::RoleClient>,
        server_name: &str,
        entry: &SkillMetadata,
        request_uri: &str,
    ) -> Option<(String, Option<String>)> {
        let handle: HandleToken = match self.registry.discovery_state(server_name) {
            Some(ServerDiscoveryState::Discovered { handle, .. }) => handle,
            _ => return None, // Started/无状态：不恢复
        };
        let entry_uri = match &entry.origin {
            Some(SkillOrigin::Mcp { uri, .. }) => uri.clone(),
            None => return None, // locate_skill_binding 保证 Mcp origin，防御
        };
        let (meta, content, mime) =
            refresh_entry_and_content(peer, server_name, &entry_uri, request_uri).await?;
        // 回写：按 origin.uri 定位原条目并**保留其原 name**（刷新
        // description/content/resources——恢复内容是同一技能的当前快照，
        // 不重跑 disambiguate_names；若直接替换为新条目的 name，会绕过消歧
        // 造成注册名漂移/撞名）。定位不到 → 不回写：skills/get 对已删技能
        // 仍应答，追加会复活已删条目（debug 日志）。
        let mut refreshed_entries = self.registry.skills_of(server_name);
        let Some(slot) = refreshed_entries.iter_mut().find(
            |e| matches!(&e.origin, Some(SkillOrigin::Mcp { uri: u, .. }) if u == &entry_uri),
        ) else {
            tracing::debug!(
                server = %server_name,
                %request_uri,
                "MCP skill 热更新恢复成功但条目已不在注册表（已删除），跳过回写"
            );
            return Some((content, mime));
        };
        let original_name = slot.name.clone();
        *slot = meta;
        slot.name = original_name;
        if self
            .registry
            .refresh_entries(server_name, &handle, refreshed_entries)
        {
            tracing::debug!(
                server = %server_name,
                %request_uri,
                "MCP skill 热更新恢复成功，registry 条目已刷新"
            );
        } else {
            // handle 不一致（恢复期间重连/新发现）→ 不回写；内容已通过全量
            // 校验，仍返回（避免 agent 反复读同一陈旧内容）。
            tracing::warn!(
                server = %server_name,
                %request_uri,
                "MCP skill 热更新恢复成功但 registry 回写被拒（handle 不一致），条目未更新"
            );
        }
        Some((content, mime))
    }
}

#[async_trait]
impl BaseTool for McpResourceTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "MCP 服务器名称（配置中的 key）"
                },
                "uri": {
                    "type": "string",
                    "description": "要读取的资源 URI"
                }
            },
            "required": ["server_name", "uri"]
        })
    }

    fn description(&self) -> &str {
        &self.cached_description
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // 1. 提取参数
        let server_name = input
            .get("server_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResourceError::InvalidParam("缺少 server_name 参数".into()))?;
        let uri = input
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResourceError::InvalidParam("缺少 uri 参数".into()))?;

        // 2. 获取客户端句柄
        let handle = self
            .client_pool
            .get_client(server_name)
            .ok_or_else(|| ResourceError::ServerNotFound {
                server: server_name.to_string(),
            })?
            .clone();

        // 3. 检查连接状态
        if !matches!(handle.status, ClientStatus::Connected) {
            return Err(Box::new(ResourceError::NotConnected {
                server: server_name.to_string(),
                status: handle.status.clone(),
            }));
        }

        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| ResourceError::NotConnected {
                server: server_name.to_string(),
                status: ClientStatus::Disconnected,
            })?;

        // 4. skill:// 资源完整性绑定定位（规范 MUST：host 持有某 skill 的
        //    entry 期间，读该 skill 的文件须 resolve 到 entry.resources 所列
        //    URI；读未列出文件 = 验证失败，等同 digest 不匹配）。
        //
        //    边界（注释写明）：
        //    - 无任何条目覆盖该 uri（未列举技能 / 普通 skill:// 资源）→
        //      不校验，保持现状普通读；
        //    - 条目 resources 为空（动态生成技能，规范 MAY 省略）→ 无法内容
        //      绑定，不校验；
        //    - Unlisted（覆盖但未列出）→ 读前拒绝，但先尝试热更新恢复
        //      （新条目可能已列出该 uri）；
        //    - Listed → 读后逐内容 digest 校验，失败走热更新恢复。
        let entries = self.registry.skills_of(server_name);
        let binding = if is_skill_scheme(uri) {
            locate_skill_binding(&entries, uri)
        } else {
            None
        };

        // Unlisted：读前拒绝——先尝试热更新恢复（skills/get 刷新单条目，
        // 新条目可能已列出该 uri）；恢复失败 → 验证失败（MUST NOT use）。
        if let Some((entry, SkillBinding::Unlisted)) = binding {
            if let Some((content, mime)) = self
                .recover_via_refresh(peer, server_name, entry, uri)
                .await
            {
                return Ok(format_recovered_content(&content, mime.as_deref()));
            }
            return Err(Box::new(ResourceError::VerificationFailed {
                server: server_name.to_string(),
                reason: format!("资源 {uri} 未列入技能条目的 resources 清单（内容绑定校验失败）"),
            }));
        }

        // 5. 调用 rmcp read_resource
        let result = tokio::time::timeout(
            RESOURCE_READ_TIMEOUT,
            self.client_pool
                .read_resource_cached(server_name, uri, peer),
        )
        .await;

        match result {
            Ok(Ok((resource_result, cache_ticket))) => {
                // 6. digest 校验（Listed，MUST）：读到的内容须与条目 resources
                //    声明的 sha256 digest 一致——contents **逐项**校验（Text
                //    用 UTF-8 bytes，Blob 用 base64 解码后的 bytes）；任一
                //    不匹配 / 无内容 → 验证失败（MUST NOT use，内容被替换/
                //    陈旧）。失败 → 热更新恢复（skills/get 刷新单条目 → 全量
                //    校验 → 回写 registry）→ 成功返回新内容，失败保持拒绝。
                //
                //    多 contents 语义（保守口径）：多 contents 场景以请求 uri
                //    的 digest 逐项校验，任一项不匹配即拒绝；规范上多 content
                //    各属不同 uri（各自 digest），若后续需要可改为按
                //    content.uri 查各自 digest。
                if let Some((entry, SkillBinding::Listed(digest))) = binding {
                    let all_match = !resource_result.contents.is_empty()
                        && resource_result
                            .contents
                            .iter()
                            .all(|content| match content {
                                rmcp::model::ResourceContents::TextResourceContents {
                                    text,
                                    ..
                                } => verify_digest(text, digest),
                                rmcp::model::ResourceContents::BlobResourceContents {
                                    blob,
                                    ..
                                } => verify_blob_digest(blob, digest),
                                _ => false,
                            });
                    if !all_match {
                        if let Some((content, mime)) = self
                            .recover_via_refresh(peer, server_name, entry, uri)
                            .await
                        {
                            return Ok(format_recovered_content(&content, mime.as_deref()));
                        }
                        return Err(Box::new(ResourceError::VerificationFailed {
                            server: server_name.to_string(),
                            reason: format!(
                                "资源 {uri} 内容与技能条目声明的 sha256 digest 不一致（内容被替换或陈旧）"
                            ),
                        }));
                    }
                }
                // 7. 仅在内容绑定校验成功（或该资源不受绑定约束）后，才允许
                //    public 响应进入跨进程缓存。校验失败及恢复失败路径均不会落盘。
                self.client_pool
                    .cache_verified_resource(cache_ticket, &resource_result)
                    .await;
                // 8. 格式化资源内容（截断超大输出）
                let mut output = Vec::new();
                for content in &resource_result.contents {
                    match content {
                        rmcp::model::ResourceContents::TextResourceContents {
                            text,
                            mime_type,
                            ..
                        } => {
                            let mime = mime_type.as_deref().unwrap_or("plain");
                            output.push(format!("[text/{}]", mime));
                            output.push(text.clone());
                        }
                        rmcp::model::ResourceContents::BlobResourceContents {
                            blob,
                            mime_type,
                            ..
                        } => {
                            let mime = mime_type.as_deref().unwrap_or("octet-stream");
                            output.push(format!("[blob/{}]", mime));
                            output.push(format!("<{} bytes of binary data>", blob.len()));
                        }
                        _ => {}
                    }
                }
                let formatted = output.join("\n");
                Ok(format_lines(formatted))
            }
            Ok(Err(e)) => Err(Box::new(ResourceError::ReadFailed {
                server: server_name.to_string(),
                reason: e.to_string(),
            })),
            Err(_) => Err(Box::new(ResourceError::ReadFailed {
                server: server_name.to_string(),
                reason: format!("资源读取超时 ({}s)", RESOURCE_READ_TIMEOUT.as_secs()),
            })),
        }
    }
}

/// skill:// 完整性绑定的定位结果（[`locate_skill_binding`]）。
#[derive(Debug, Copy, Clone)]
enum SkillBinding<'a> {
    /// uri 已列入条目 resources → 期望 digest（读后 sha256 校验）
    Listed(&'a str),
    /// 条目覆盖该 uri 但未列入 resources → 验证失败（规范 MUST）
    Unlisted,
    /// 条目 resources 为空（动态生成技能，规范 MAY 省略）→ 不校验
    Unbound,
}

/// 纯函数：URI 是否位于 skill 根（`root` 不带尾 "/"，scheme 大小写不敏感）
/// 之下——uri == root，或以 root + "/" 为边界前缀（"skill://a" 不覆盖
/// "skill://ab/..."）。scheme 大小写不敏感比较复用 skill_discovery 的
/// `uri_eq_ignore_scheme_case`（读取面与发现侧恢复路径共用）。
fn uri_under_skill_root(uri: &str, root: &str) -> bool {
    if uri_eq_ignore_scheme_case(uri, root) {
        return true;
    }
    uri.len() > root.len()
        && uri
            .get(..8)
            .zip(root.get(..8))
            .is_some_and(|(pa, pb)| pa.eq_ignore_ascii_case(pb))
        && uri.get(8..root.len()) == root.get(8..)
        && uri.get(root.len()..root.len() + 1) == Some("/")
}

/// 纯函数：在 server 的已发现条目中定位覆盖 `uri` 的技能条目（entry.uri
/// 去掉尾部 "/SKILL.md" 即 skill 根；uri 以根+"/" 开头或等于根）。
///
/// 边界（注释写明）：
/// - 非 `skill://` 前缀（scheme 大小写不敏感，RFC 3986）/ 无条目覆盖 →
///   None（未列举技能或普通 skill:// 资源 → 不校验，保持现状普通读）；
/// - 多条目覆盖（嵌套技能：祖先与子孙 SKILL.md 的根都包含该 uri）→ 取
///   root **最长**的（更具体的条目；根边界以 "/" 为界——"skill://a" 不
///   覆盖 "skill://ab/..."）。
fn locate_skill_binding<'a>(
    entries: &'a [SkillMetadata],
    uri: &str,
) -> Option<(&'a SkillMetadata, SkillBinding<'a>)> {
    if !is_skill_scheme(uri) {
        return None;
    }
    let mut best: Option<(&'a SkillMetadata, usize)> = None;
    for entry in entries {
        let Some(SkillOrigin::Mcp { uri: entry_uri, .. }) = &entry.origin else {
            continue;
        };
        let Some(root) = entry_uri.strip_suffix("/SKILL.md") else {
            continue;
        };
        if uri_under_skill_root(uri, root)
            && best.is_none_or(|(_, best_root)| root.len() > best_root)
        {
            best = Some((entry, root.len()));
        }
    }
    let (entry, _) = best?;
    let binding = if entry.resources.is_empty() {
        SkillBinding::Unbound
    } else if let Some(r) = entry
        .resources
        .iter()
        .find(|r| uri_eq_ignore_scheme_case(&r.uri, uri))
    {
        SkillBinding::Listed(r.digest.as_str())
    } else {
        SkillBinding::Unlisted
    };
    Some((entry, binding))
}

/// 纯函数：Blob 内容 digest 校验——rmcp 3.1.2 的 `BlobResourceContents.blob`
/// 是 **base64 编码字符串**（解码后即 raw bytes，与 server 计算 digest 的
/// 字节一致）。非法 base64 → 无法校验，视为不匹配（MUST NOT use）。
fn verify_blob_digest(blob: &str, expected: &str) -> bool {
    match base64::engine::general_purpose::STANDARD.decode(blob) {
        Ok(bytes) => verify_digest_bytes(&bytes, expected),
        Err(_) => false,
    }
}

/// 工具输出格式化：超过 `MAX_MCP_LINES` 行 → 截断 + 持久化提示（正常路径
/// 与热更新恢复路径共用，保证截断语义一致）。
fn format_lines(formatted: String) -> String {
    let lines: Vec<&str> = formatted.lines().collect();
    if lines.len() > MAX_MCP_LINES {
        let persist_hint = persist_truncated_output(&formatted);
        let truncated: String = lines[..MAX_MCP_LINES].join("\n");
        format!(
            "{truncated}\n\n[MCP output truncated: {} total lines]{persist_hint}",
            lines.len()
        )
    } else {
        formatted
    }
}

/// 热更新恢复成功的内容格式化：对齐正常路径的 `[text/<mime>]` 前缀
/// （mime 取自重读的 ResourceContents 原值，缺省 plain）与行数截断/
/// 持久化（复用 [`format_lines`]）。
fn format_recovered_content(content: &str, mime: Option<&str>) -> String {
    let mime = mime.unwrap_or("plain");
    format_lines(format!("[text/{mime}]\n{content}"))
}

#[cfg(test)]
#[path = "resource_tool_test.rs"]
mod tests;
