//! Agent 定义资源面（project / plugin 来源；builtin 静态表属 W5 迁移）。
//!
//! 语义口径（W1 冻结）：
//! - 目录布局与既有 `scan_agents_detailed` 同形：`{root}/*.md`（文件 stem =
//!   agent 标识）或 `{root}/{id}/agent.md`（目录形态）；
//! - URI 的 `{name}` 段 = agent 定义标识（文件 stem / 目录名），**不是**
//!   frontmatter display name——宿主 `McpAgentRegistry` / SubAgent 消费面以
//!   标识为键，本面与之一致；
//! - 只做最小 frontmatter 解析（`name` / `description` 供 `resources/list`
//!   的展示字段）；Agent 的完整解析、权限收敛与批准语义归宿主（W5），本面
//!   不做能力解释、不因 frontmatter 声称而提权；
//! - 公开面禁 symlink：目录与定义文件都拒绝 symlink（与技能面同口径）；
//! - agents 不发明 `agents/list|get`：发现走 `resources/list`，读取走
//!   `resources/read`（MCPP：Agent 用标准 resources）。

use std::path::{Path, PathBuf};

use peri_acp_types::workspace_resources::{
    agent_uri, digest_bytes, is_valid_uri_segment, ResourceScope, AGENT_ENTRY_FILE,
};

use super::frontmatter::frontmatter_json;
use super::path::{canonical_root, is_text_bytes};
use super::scan::is_regular_file;
use super::{ResourceBudget, ResourceError, WorkspaceResourcesInput};

/// 已扫描 agent 条目。
pub(crate) struct AgentRecord {
    pub scope: ResourceScope,
    pub plugin_name: Option<String>,
    pub agent_id: String,
    pub display_name: String,
    pub description: String,
    /// canonical 根（读取用）。
    pub root: PathBuf,
    /// 定义文件相对根的路径。
    pub relative_path: String,
    /// 定义文件 digest（读取时复算，不缓存）。
    pub digest: String,
}

impl AgentRecord {
    pub fn uri(&self) -> Option<String> {
        agent_uri(self.scope, self.plugin_name.as_deref(), &self.agent_id)
    }
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
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());

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
        let frontmatter = frontmatter_json(text).unwrap_or_default();
        let display_name = frontmatter
            .get("name")
            .and_then(|value| value.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&agent_id)
            .to_string();
        let description = frontmatter
            .get("description")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        records.push(AgentRecord {
            scope,
            plugin_name: plugin_name.map(str::to_string),
            agent_id,
            display_name,
            description,
            root: root_canonical.to_path_buf(),
            relative_path,
            digest: digest_bytes(&bytes),
        });
    }
    records
}

/// 跨 agent 根组装目录（输入顺序即优先级，同一 agent 标识先到先得）。
pub(crate) fn scan_catalog(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
) -> Vec<AgentRecord> {
    let mut catalog: Vec<AgentRecord> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for root in &input.agent_roots {
        let Some(canonical) = canonical_root(&root.path) else {
            continue;
        };
        for record in scan_root(&canonical, root.scope, root.plugin_name.as_deref(), budget) {
            if !seen.insert(record.agent_id.clone()) {
                tracing::debug!(
                    agent = %record.agent_id,
                    scope = record.scope.as_str(),
                    "同名 agent 已被更高优先级来源选中，shadow 项不公开"
                );
                continue;
            }
            catalog.push(record);
        }
    }
    catalog
}

/// 按 URI 定位 agent 定义（scope/plugin 匹配的根内按标识查找；读取用）。
pub(crate) fn locate(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
    scope: ResourceScope,
    plugin_name: Option<&str>,
    agent_id: &str,
) -> Result<AgentRecord, ResourceError> {
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
