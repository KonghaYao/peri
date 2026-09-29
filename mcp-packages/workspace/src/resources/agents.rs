//! Agent 定义资源面（project / plugin / builtin 三类来源；W5 迁入 builtin 静态表）。
//!
//! 语义口径（W1 冻结 + W5 扩展）：
//! - 目录布局与既有 `scan_agents_detailed` 同形：`{root}/*.md`（文件 stem =
//!   agent 标识）或 `{root}/{id}/agent.md`（目录形态）；
//! - URI 的 `{name}` 段 = agent 定义标识（文件 stem / 目录名），**不是**
//!   frontmatter display name——宿主 `McpAgentRegistry` / SubAgent 消费面以
//!   标识为键，本面与之一致；
//! - 只做最小 frontmatter 解析（`name` / `description` 供 `resources/list`
//!   的展示字段；完整 frontmatter 经 `_meta` 逐字投影，见下）；Agent 的完整
//!   解析、权限收敛与批准语义归宿主（W5），本面不做能力解释、不因 frontmatter
//!   声称而提权；
//! - **跨来源同名并存（W5）**：project / builtin / plugin 三个来源的同名定义
//!   各自是可读资源（URI authority 不同），本面**不做跨来源 shadow**；选择
//!   优先级（E13 project→builtin→plugin）与启用位是宿主的会话策略，由宿主在
//!   消费面（目录渲染 / 定义加载）判定。同一来源根（scope + plugin）内同名
//!   仍先到先得；
//! - `_meta` 投影：scope / plugin / digest 与技能面同键，另加
//!   [`META_KEY_FRONTMATTER`]（frontmatter 的 JSON 对象，逐字不改写）——宿主
//!   目录渲染据此推断 model tier / 写能力标签，**不读正文**（§6.2「只把候选
//!   描述投递主模型」；正文仅在激活时经 `resources/read` 读取）；
//! - 公开面禁 symlink：目录与定义文件都拒绝 symlink（与技能面同口径）；
//! - agents 不发明 `agents/list|get`：发现走 `resources/list`，读取走
//!   `resources/read`（MCPP：Agent 用标准 resources）。

use std::path::{Path, PathBuf};

use peri_acp_types::workspace_resources::{
    agent_uri, digest_bytes, is_valid_uri_segment, ResourceScope, AGENT_ENTRY_FILE,
    META_KEY_FRONTMATTER,
};

use super::frontmatter::frontmatter_json;
use super::path::{canonical_root, is_text_bytes};
use super::scan::is_regular_file;
use super::{ResourceBudget, ResourceError, WorkspaceResourcesInput};

/// agent 定义的存储位置。
#[derive(Debug, Clone)]
pub(crate) enum AgentStore {
    /// 磁盘来源：canonical 根 + 定义文件相对根的路径。
    Disk {
        root: PathBuf,
        relative_path: String,
    },
    /// builtin 静态资产：`builtin::BUILTIN_AGENTS` 的下标。
    Builtin { index: usize },
}

impl AgentStore {
    /// 定义文件相对根的路径（builtin 为文件名；仅测试断言使用）。
    #[cfg(test)]
    pub fn relative_path(&self) -> &str {
        match self {
            Self::Disk { relative_path, .. } => relative_path,
            Self::Builtin { .. } => AGENT_ENTRY_FILE,
        }
    }
}

/// 已扫描 agent 条目。
pub(crate) struct AgentRecord {
    pub scope: ResourceScope,
    pub plugin_name: Option<String>,
    pub agent_id: String,
    pub display_name: String,
    pub description: String,
    pub store: AgentStore,
    /// frontmatter JSON（可为 `None`：无 frontmatter / 非 mapping）；逐字投影。
    pub frontmatter: Option<serde_json::Map<String, serde_json::Value>>,
    /// 定义文件 digest（读取时复算，不缓存）。
    pub digest: String,
}

impl AgentRecord {
    pub fn uri(&self) -> Option<String> {
        agent_uri(self.scope, self.plugin_name.as_deref(), &self.agent_id)
    }

    /// `_meta` 投影（含 frontmatter JSON；无 frontmatter 时不写该键）。
    pub fn meta(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut meta = serde_json::Map::new();
        if let Some(frontmatter) = &self.frontmatter {
            meta.insert(
                META_KEY_FRONTMATTER.to_string(),
                serde_json::Value::Object(frontmatter.clone()),
            );
        }
        meta
    }
}

/// 从定义文本构造展示字段与 frontmatter 投影（`None` = 无合法 frontmatter）。
fn frontmatter_parts(text: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    frontmatter_json(text)
}

/// 扫描一个 agent 根（canonical）下的全部定义（顺序确定：按条目名排序）。
pub(crate) fn scan_root(
    root_canonical: &Path,
    scope: ResourceScope,
    plugin_name: Option<&str>,
    budget: &ResourceBudget,
) -> Vec<AgentRecord> {
    let mut records = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir(root_canonical) else {
        return records;
    };
    // 同 id 两形态（`{id}/agent.md` 目录形态 vs `{id}.md` 文件形态）的优先级：
    // **目录形态先**（与迁移前候选序一致，F13）。顺序由排序键显式表达，并配合
    // 下方 `seen` 去重——不依赖文件名字典序巧合。
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir_form = std::fs::metadata(entry.path())
            .map(|m| m.is_dir())
            .unwrap_or(false);
        // 目录形态排前（false < true）。
        (!is_dir_form, name)
    });

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if super::path::is_hidden_name(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 目录形态 `{id}/agent.md`；文件形态 `{id}.md`。symlink 一律跳过。
        let (agent_id, relative_path) = if file_type.is_file() {
            let Some(stem) = Path::new(name.as_ref())
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
            else {
                continue;
            };
            if Path::new(name.as_ref())
                .extension()
                .and_then(|ext| ext.to_str())
                != Some("md")
            {
                continue;
            }
            (stem, name.to_string())
        } else if file_type.is_dir() {
            let agent_file = entry.path().join(AGENT_ENTRY_FILE);
            if !is_regular_file(&agent_file) {
                continue;
            }
            (name.to_string(), format!("{}/{AGENT_ENTRY_FILE}", name))
        } else {
            continue;
        };
        if !is_valid_uri_segment(&agent_id) || !seen.insert(agent_id.clone()) {
            tracing::debug!(agent = %agent_id, "agent 标识非法或重复，跳过");
            continue;
        }
        let Some(bytes) = read_definition(root_canonical, &relative_path, budget) else {
            tracing::debug!(agent = %agent_id, "agent 定义不可读或超预算，跳过");
            continue;
        };
        let Some(text) = std::str::from_utf8(&bytes).ok() else {
            tracing::debug!(agent = %agent_id, "agent 定义非 UTF-8 文本，跳过");
            continue;
        };
        let (display_name, description) = display_fields(&agent_id, text);
        records.push(AgentRecord {
            scope,
            plugin_name: plugin_name.map(str::to_string),
            agent_id,
            display_name,
            description,
            store: AgentStore::Disk {
                root: root_canonical.to_path_buf(),
                relative_path,
            },
            frontmatter: frontmatter_parts(text),
            digest: digest_bytes(&bytes),
        });
    }
    records
}

/// builtin 静态 Agent 记录（`agent://builtin/{id}/agent.md`）。
///
/// 预算超限 / 标识非法的条目跳过并记日志（与技能面 builtin 同口径）。
pub(crate) fn builtin_records(budget: &ResourceBudget) -> Vec<AgentRecord> {
    let mut records = Vec::new();
    for (index, agent) in super::builtin::BUILTIN_AGENTS.iter().enumerate() {
        let bytes = agent.content.as_bytes();
        if bytes.len() as u64 > budget.max_file_bytes {
            tracing::warn!(agent = agent.id, "builtin agent 超过大小预算，跳过");
            continue;
        }
        if !is_valid_uri_segment(agent.id) {
            tracing::warn!(
                agent = agent.id,
                "builtin agent 标识不符合 URI 段约束，不公开"
            );
            continue;
        }
        let (display_name, description) = display_fields(agent.id, agent.content);
        records.push(AgentRecord {
            scope: ResourceScope::Builtin,
            plugin_name: None,
            agent_id: agent.id.to_string(),
            display_name,
            description,
            store: AgentStore::Builtin { index },
            frontmatter: frontmatter_parts(agent.content),
            digest: digest_bytes(bytes),
        });
    }
    records
}

/// 展示字段：frontmatter `name`（空/缺失回退标识）与 trim 后的 `description`。
fn display_fields(agent_id: &str, text: &str) -> (String, String) {
    let frontmatter = frontmatter_json(text).unwrap_or_default();
    let display_name = frontmatter
        .get("name")
        .and_then(|value| value.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(agent_id)
        .to_string();
    let description = frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    (display_name, description)
}

/// 组装目录：宿主输入根（顺序即公开顺序） + builtin 静态表（W5）。
///
/// **不做跨来源 shadow**（W5/§6.2「跨 origin 同名并存」）：project / builtin /
/// plugin 同名定义各自公开、各自可读；选择优先级（E13 project→builtin→plugin）
/// 与启用位由宿主在消费面判定。同一来源根（scope + plugin）内同名仍先到先得。
pub(crate) fn scan_catalog(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
) -> Vec<AgentRecord> {
    let mut catalog: Vec<AgentRecord> = Vec::new();
    let mut seen: std::collections::HashSet<(ResourceScope, Option<String>, String)> =
        std::collections::HashSet::new();
    for root in &input.agent_roots {
        let Some(canonical) = canonical_root(&root.path) else {
            continue;
        };
        for record in scan_root(&canonical, root.scope, root.plugin_name.as_deref(), budget) {
            if !seen.insert((
                record.scope,
                record.plugin_name.clone(),
                record.agent_id.clone(),
            )) {
                tracing::debug!(
                    agent = %record.agent_id,
                    scope = record.scope.as_str(),
                    "同一来源根内同名 agent 先到先得，重复项不公开"
                );
                continue;
            }
            catalog.push(record);
        }
    }
    catalog.extend(builtin_records(budget));
    catalog
}

/// 按 URI 定位 agent 定义（scope/plugin 匹配的根内按标识查找；读取用）。
///
/// builtin scope 直接查静态表（W5；`Builtin` 不是磁盘根，`plugin_name` 必须为空）。
pub(crate) fn locate(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
    scope: ResourceScope,
    plugin_name: Option<&str>,
    agent_id: &str,
) -> Result<AgentRecord, ResourceError> {
    if scope == ResourceScope::Builtin {
        if plugin_name.is_some() {
            return Err(ResourceError::InvalidUri);
        }
        return builtin_records(budget)
            .into_iter()
            .find(|record| record.agent_id == agent_id)
            .ok_or(ResourceError::NotFound);
    }
    let mut matched_scope_root = false;
    for root in &input.agent_roots {
        if root.scope != scope || root.plugin_name.as_deref() != plugin_name {
            continue;
        }
        let Some(canonical) = canonical_root(&root.path) else {
            continue;
        };
        matched_scope_root = true;
        if let Some(record) = scan_root(&canonical, root.scope, root.plugin_name.as_deref(), budget)
            .into_iter()
            .find(|record| record.agent_id == agent_id)
        {
            return Ok(record);
        }
    }
    if matched_scope_root {
        Err(ResourceError::NotFound)
    } else {
        Err(ResourceError::InvalidUri)
    }
}

/// 读取 agent 定义原文（UTF-8 文本；返回原始字节，digest 由调用方一并持有）。
///
/// 磁盘来源经 `safe_read_file`（symlink / 越界 / 预算校验）；builtin 来源读
/// 编译期嵌入字节（无 IO）。
pub(crate) fn read_record_definition(
    record: &AgentRecord,
    budget: &ResourceBudget,
) -> Option<Vec<u8>> {
    let bytes = match &record.store {
        AgentStore::Disk {
            root,
            relative_path,
        } => super::path::safe_read_file(root, relative_path, budget.max_file_bytes).ok()?,
        AgentStore::Builtin { index } => super::builtin::builtin_agent_bytes(*index)?.to_vec(),
    };
    if bytes.len() as u64 > budget.max_file_bytes || !is_text_bytes(&bytes) {
        return None;
    }
    Some(bytes)
}

/// 读取 agent 定义原文（根 + 相对路径形态；兼容既有调用点）。
pub(crate) fn read_definition(
    root_canonical: &Path,
    relative_path: &str,
    budget: &ResourceBudget,
) -> Option<Vec<u8>> {
    let bytes =
        super::path::safe_read_file(root_canonical, relative_path, budget.max_file_bytes).ok()?;
    if !is_text_bytes(&bytes) {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
#[path = "agents_test.rs"]
mod tests;
