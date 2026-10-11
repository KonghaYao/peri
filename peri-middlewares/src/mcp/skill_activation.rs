//! 统一 skill 激活（W2）：tool / slash-RPC / preload 共用的唯一 MCP 正文读取点。
//!
//! 数据路径（plan §5.3）：发现只发布 metadata（`skills/list` 阶段不读正文）→
//! 激活：权限 → `resources/read` → digest/frontmatter 校验 → origin 标记 →
//! 注入。校验规则与 SEP-2640 一致：
//!
//! - digest 必须与条目 `resources[]` 中该 URI 的 `sha256:` 声明逐字节一致；
//! - 正文 frontmatter 必须与条目快照**逐字段全量**一致（任何差异，含附加
//!   字段，拒绝加载）；
//! - digest 失败是 stale 信号：经 `skills/get` 刷新条目后重读一次（仅一次）；
//!   frontmatter/身份失败不是 stale，不重试。
//!
//! 断连与换代：peer 每次激活都从 registry 的**当前** handle 解析（`Arc::ptr_eq`
//! 语义由 registry 的发现完成回写保证）——旧代 handle 的晚到结果不会复活，
//! 因为条目本身已被重连重扫覆盖。
//!
//! legacy 兜底：server 未声明 Skills 扩展时条目无 `resources[]` 绑定，激活
//! 退化为「使用发现阶段已校验的正文」（该路径的正文读取仍在发现期完成）。
//!
//! 批准面（X6 核实结论，2026-09-29 W2b）：**skill 级批准面当前不存在**——
//! 既有批准只落在两处：(a) 工具粒度 `permission::sensitive_tool_entries()`
//! （14 项 + 3 前缀，含 `mcp__` 前缀「按绑定来源审批」；`SkillTool` 不在表内，
//! 属免批准 direct 工具）；(b) agent 粒度 `McpAgentRegistry::approvals`
//! （内容 + 有效能力绑定，变更重批）。因此 X6 的「本地受信来源免逐技能批准」
//! 在 W2 无面可删（本来就不逐技能弹窗），「远端维持既有批准面」= 沿用上述
//! 工具/agent 两处；**受信来源的完整性校验不可豁免**（免批准 ≠ 免校验），
//! 由本模块的统一 activation 保证（见 `trusted_origin_still_verifies_content_binding` 用例）。
//! skill 级内容绑定批准属 W3/W4 范围（需先有面），本波不新造批准面。

use peri_acp_types::{
    mcp_skills::{HandleToken, McpSkillRegistry, ServerDiscoveryState},
    skills::{SkillMetadata, SkillOrigin},
};
use rmcp::{service::Peer, RoleClient};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::client::McpClientHandle;
use super::skill_discovery::{
    frontmatter_maps_equal, parse_skill_frontmatter_map, read_skill_resource_text,
    recover_entry_via_skills_get, uri_eq_ignore_scheme_case, verify_digest, SkillResourceRead,
};

/// 激活失败原因（调用方据此决定文案/降级；不含主机绝对路径与正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivationError {
    /// 条目不是 MCP 来源（本地/内置 skill 走各自的读取路径）。
    NotMcpSourced,
    /// server 断连、当前 handle 无 peer 或不在 registry 中。
    Unreachable,
    /// 条目无 SKILL.md 内容绑定（动态技能或 legacy 兜底条目）。
    MissingBinding,
    /// `resources/read` 失败或超时。
    ReadFailed,
    /// 读取返回的不是文本内容（Blob）。
    NotText,
    /// 取消。
    Cancelled,
    /// digest 与条目声明不一致，且 `skills/get` 刷新一次后仍不一致。
    DigestMismatch,
    /// 正文 frontmatter 与条目快照不一致（陈旧或篡改），不重试。
    FrontmatterMismatch,
}

impl ActivationError {
    /// 面向用户/模型的简短原因（不含正文与绝对路径）。
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::NotMcpSourced => "not an MCP-sourced skill",
            Self::Unreachable => "MCP server is not connected",
            Self::MissingBinding => "skill entry has no content binding",
            Self::ReadFailed => "skill resource read failed",
            Self::NotText => "skill resource is not text",
            Self::Cancelled => "cancelled",
            Self::DigestMismatch => "skill content digest mismatch",
            Self::FrontmatterMismatch => "skill frontmatter mismatch",
        }
    }
}

/// 从 registry 的**当前** handle 解析 server 的 peer。
///
/// 返回 `(handle, peer)`：handle 用于刷新回写（`refresh_entries` 的代数校验），
/// peer 用于本次读取。registry 无该 server / 已断连 / handle 非本 crate 的
/// 客户端类型 → `None`（调用方按 `Unreachable` 处理，不回落磁盘）。
fn peer_of(registry: &McpSkillRegistry, server: &str) -> Option<(HandleToken, Peer<RoleClient>)> {
    let state = registry.discovery_state(server)?;
    let handle = match &state {
        ServerDiscoveryState::Started { handle }
        | ServerDiscoveryState::Discovered { handle, .. } => handle.clone(),
        ServerDiscoveryState::Failed { .. } => return None,
    };
    let client = handle.clone().downcast::<McpClientHandle>().ok()?;
    let peer = client.peer.clone()?;
    Some((handle, peer))
}

/// 激活 skill：返回**已校验**的 SKILL.md 正文。
///
/// 调用方负责 origin 标注（`annotate_mcp_content`）与注入；本函数不写缓存、
/// 不做批准（批准按 origin 分级，见 X6，在各自的入口层完成）。
pub(crate) async fn activate(
    registry: &McpSkillRegistry,
    meta: &SkillMetadata,
    cancel: Option<&AgentCancellationToken>,
) -> Result<String, ActivationError> {
    if cancel.is_some_and(|token| token.is_cancelled()) {
        return Err(ActivationError::Cancelled);
    }
    let Some(SkillOrigin::Mcp { server, uri }) = &meta.origin else {
        return Err(ActivationError::NotMcpSourced);
    };
    // 绑定面：条目 resources[] 中 SKILL.md 自身条目 + 发现时 frontmatter 快照。
    let binding = meta
        .resources
        .iter()
        .find(|resource| uri_eq_ignore_scheme_case(&resource.uri, uri));
    let (Some(binding), Some(expected_fm)) = (binding, meta.frontmatter.as_ref()) else {
        // legacy 兜底（无绑定条目）：使用发现阶段已校验的正文。
        return match &meta.content {
            Some(content) => Ok(content.clone()),
            None => Err(ActivationError::MissingBinding),
        };
    };
    let Some((handle, peer)) = peer_of(registry, server) else {
        return Err(ActivationError::Unreachable);
    };
    let text = read_text(&peer, server, uri).await?;
    match verify_body(&text, &binding.digest, expected_fm) {
        Ok(()) => Ok(text),
        Err(ActivationError::DigestMismatch) => {
            refresh_after_digest_mismatch(registry, &peer, server, uri, &handle, cancel).await
        }
        Err(other) => Err(other),
    }
}

/// stale 恢复：`skills/get` 刷新条目一次 → 按刷新后的绑定重读并全量校验 →
/// 回写 registry（仅替换该条目，保留同 server 其他条目）。失败 → `DigestMismatch`
/// （保留原条目，不写半成品）。
async fn refresh_after_digest_mismatch(
    registry: &McpSkillRegistry,
    peer: &Peer<RoleClient>,
    server: &str,
    uri: &str,
    handle: &HandleToken,
    cancel: Option<&AgentCancellationToken>,
) -> Result<String, ActivationError> {
    let Some(refreshed) = recover_entry_via_skills_get(peer, server, uri).await else {
        return Err(ActivationError::DigestMismatch);
    };
    if cancel.is_some_and(|token| token.is_cancelled()) {
        return Err(ActivationError::Cancelled);
    }
    // 按刷新后的条目快照**读一次**正文并做 digest/frontmatter 全量校验；失败即
    // 拒绝（不写半成品、不回退旧内容、不再额外重读）。
    let binding = refreshed
        .resources
        .iter()
        .find(|resource| uri_eq_ignore_scheme_case(&resource.uri, uri))
        .ok_or(ActivationError::MissingBinding)?;
    let expected_fm = refreshed
        .frontmatter
        .as_ref()
        .ok_or(ActivationError::MissingBinding)?;
    let text = read_text(peer, server, uri).await?;
    verify_body(&text, &binding.digest, expected_fm)?;
    write_back(registry, server, handle, refreshed);
    Ok(text)
}

/// 单次 `resources/read` → 文本；Blob / 失败 / 超时分别映射为错误。
async fn read_text(
    peer: &Peer<RoleClient>,
    server: &str,
    uri: &str,
) -> Result<String, ActivationError> {
    match read_skill_resource_text(peer, server, uri).await {
        SkillResourceRead::Text(text, ..) => Ok(text),
        SkillResourceRead::NotText => Err(ActivationError::NotText),
        SkillResourceRead::Failed => Err(ActivationError::ReadFailed),
    }
}

/// digest + frontmatter 全量校验（纯函数；错误分类即 stale 判定依据）。
fn verify_body(
    text: &str,
    expected_digest: &str,
    expected_fm: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ActivationError> {
    if !verify_digest(text, expected_digest) {
        return Err(ActivationError::DigestMismatch);
    }
    let Some(actual_fm) = parse_skill_frontmatter_map(text) else {
        return Err(ActivationError::FrontmatterMismatch);
    };
    if !frontmatter_maps_equal(&actual_fm, expected_fm) {
        return Err(ActivationError::FrontmatterMismatch);
    }
    Ok(())
}

/// 刷新条目回写：替换同 (server, 注册名) 的条目，其余保留；代数不符（重连）
/// 时 `refresh_entries` 拒绝写入（返回 false，不影响本次返回值）。
fn write_back(
    registry: &McpSkillRegistry,
    server: &str,
    handle: &HandleToken,
    refreshed: SkillMetadata,
) {
    let mut entries = registry.skills_of(server);
    match entries
        .iter_mut()
        .find(|entry| entry.name == refreshed.name)
    {
        Some(slot) => *slot = refreshed,
        None => entries.push(refreshed),
    }
    registry.refresh_entries(server, handle, entries);
}

#[cfg(test)]
#[path = "skill_activation_test.rs"]
mod tests;
