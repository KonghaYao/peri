//! 技能资源面：本地三根 / 插件根 / builtin 静态资产的扫描、manifest 与读取。
//!
//! 语义口径（W1 冻结）：
//! - 扫描顺序即优先级（输入 roots 的顺序，宿主按 User → Global → Project →
//!   Plugin 构造；builtin 恒为最低优先级），跨 root 同名**先到先得**，shadow 项
//!   不出现在 `skills/list` / `resources/list`（X3：保持既有本地选择行为）；
//! - `skills/get` 与 `resources/read` 按 URI 的 scope **直接定位该根内的技能**
//!   （同一根内仍先到先得）——因此「未被 list 枚举的合法项」（含 shadow 项、
//!   未来分页的第二页）仍可按 URI 读取，未知项由调用方映射为 `-32602`；
//! - URI 中的技能标识 = frontmatter `name` 的逐字值；名称非法（段校验失败）
//!   的技能**不公开**并记日志（X3「非法历史名称处置」）；目录名与 `name`
//!   不一致时保留公开（存量优先）并记 debug 日志（X3 冻结的兼容口径）；
//! - 公开面禁 symlink：扫描与读取都拒绝 symlink 条目（含 `SKILL.md` 与附件）；
//! - 附件 `resources[]` 是完整清单（含 `SKILL.md` 自身），每项在扫描时读取
//!   字节算 digest；超预算的文件跳过并记日志（预算由输入控制）。

use std::path::{Path, PathBuf};

use peri_acp_types::workspace_resources::{
    digest_bytes, is_valid_uri_segment, skill_uri, ResourceScope, SkillEntry, SkillGetResponse,
    SkillResourceRef, SkillsListResponse, SKILL_ENTRY_FILE,
};
use serde_json::{Map, Value};

use super::frontmatter::{frontmatter_json, frontmatter_summary};
use super::path::{canonical_root, is_text_bytes, safe_read_file};
use super::scan::{enumerate_skill_files, find_skill_dirs, relative_path_string};
use super::{ResourceBudget, ResourceError, WorkspaceResourcesInput};

/// 技能条目的存储位置。
#[derive(Debug)]
pub(crate) enum SkillStore {
    /// 磁盘来源：canonical 根 + 技能目录相对根的路径。
    Disk { root: PathBuf, dir_relative: String },
    /// builtin 静态资产：`builtin::BUILTIN_SKILLS` 的下标。
    Builtin { index: usize },
}

/// 技能 manifest 中的一个文件。
#[derive(Debug)]
pub(crate) struct SkillFileRecord {
    pub relative_path: String,
    pub digest: String,
    pub size: u64,
    pub text: bool,
}

/// 已扫描技能条目（manifest 完整）。
#[derive(Debug)]
pub(crate) struct SkillRecord {
    pub scope: ResourceScope,
    pub plugin_name: Option<String>,
    pub name: String,
    pub description: String,
    pub store: SkillStore,
    pub frontmatter: Map<String, Value>,
    pub files: Vec<SkillFileRecord>,
}

impl SkillRecord {
    /// 技能入口 URI。
    pub fn uri(&self) -> Option<String> {
        skill_uri(
            self.scope,
            self.plugin_name.as_deref(),
            &self.name,
            SKILL_ENTRY_FILE,
        )
    }

    /// `skills/list` / `skills/get` 条目。
    pub fn entry(&self) -> Option<SkillEntry> {
        let uri = self.uri()?;
        let resources = self
            .files
            .iter()
            .map(|file| {
                Some(SkillResourceRef {
                    uri: skill_uri(
                        self.scope,
                        self.plugin_name.as_deref(),
                        &self.name,
                        &file.relative_path,
                    )?,
                    digest: file.digest.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(SkillEntry {
            uri,
            frontmatter: self.frontmatter.clone(),
            resources: Some(resources),
        })
    }

    /// 读取 manifest 中的某个文件（`relative_path` 必须已在 `files` 里）。
    pub(crate) fn read_file(
        &self,
        relative_path: &str,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, &SkillFileRecord), ResourceError> {
        let record = self
            .files
            .iter()
            .find(|file| file.relative_path == relative_path)
            .ok_or(ResourceError::NotFound)?;
        let bytes = match &self.store {
            SkillStore::Disk { root, dir_relative } => {
                let full_relative = format!("{dir_relative}/{relative_path}");
                safe_read_file(root, &full_relative, max_bytes)?
            }
            SkillStore::Builtin { index } => {
                if relative_path != SKILL_ENTRY_FILE {
                    return Err(ResourceError::NotFound);
                }
                super::builtin::builtin_skill_bytes(*index)
                    .ok_or(ResourceError::NotFound)?
                    .to_vec()
            }
        };
        Ok((bytes, record))
    }
}

/// 扫描一个根（canonical）下的全部技能（含 manifest），按文件名排序的确定性顺序。
///
/// 返回 `(技能, 被跳过的非法/失效项计数)`：失效项计数用于调用方的聚合日志。
pub(crate) fn scan_root(
    root_canonical: &Path,
    scope: ResourceScope,
    plugin_name: Option<&str>,
    budget: &ResourceBudget,
) -> Vec<SkillRecord> {
    let mut records = Vec::new();
    for dir in find_skill_dirs(root_canonical, budget.max_depth, budget.max_dirs_per_root) {
        let Some(record) = build_record(root_canonical, &dir, scope, plugin_name, budget) else {
            continue;
        };
        records.push(record);
    }
    records
}

/// 构建单个技能目录的条目；`SKILL.md` 缺失 / frontmatter 不合法 / 名称非法时 `None`。
fn build_record(
    root_canonical: &Path,
    dir: &Path,
    scope: ResourceScope,
    plugin_name: Option<&str>,
    budget: &ResourceBudget,
) -> Option<SkillRecord> {
    let dir_relative = relative_path_string(root_canonical, dir)?;
    let skill_md_relative = format!("{dir_relative}/{SKILL_ENTRY_FILE}");
    // SKILL.md 是技能身份的来源：symlink / 超预算 / 非文本 / frontmatter 不合法
    // 都使该技能不公开（跳过）。
    let skill_bytes =
        safe_read_file(root_canonical, &skill_md_relative, budget.max_file_bytes).ok()?;
    if !is_text_bytes(&skill_bytes) {
        tracing::warn!(skill_dir = %dir_relative, "技能 SKILL.md 非 UTF-8 文本，跳过");
        return None;
    }
    let skill_digest = digest_bytes(&skill_bytes);
    let skill_size = skill_bytes.len() as u64;
    let skill_text = String::from_utf8(skill_bytes).ok()?;
    let frontmatter = match frontmatter_json(&skill_text) {
        Some(frontmatter) => frontmatter,
        None => {
            tracing::warn!(skill_dir = %dir_relative, "技能 SKILL.md frontmatter 解析失败，跳过");
            return None;
        }
    };
    let Some((name, description)) = frontmatter_summary(&frontmatter) else {
        tracing::warn!(skill_dir = %dir_relative, "技能 frontmatter 缺 name/description，跳过");
        return None;
    };
    if !is_valid_uri_segment(&name) {
        // X3：非法历史名称处置 = 不公开（登记）；wire 不改写名称本身。
        tracing::warn!(skill_dir = %dir_relative, "技能名不符合 URI 段约束，不公开");
        return None;
    }

    let dir_name = dir.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if dir_name != name {
        tracing::debug!(
            skill_dir = %dir_relative,
            frontmatter_name = %name,
            "技能目录名与 frontmatter name 不一致（URI 以 frontmatter name 为准）"
        );
    }

    // 入口文件恒为 manifest 首项（用已读字节，不重读也不受枚举预算截断影响）。
    let mut files = vec![SkillFileRecord {
        relative_path: SKILL_ENTRY_FILE.to_string(),
        digest: skill_digest,
        size: skill_size,
        text: true,
    }];
    for relative_path in enumerate_skill_files(dir, budget.max_files_per_skill) {
        if relative_path == SKILL_ENTRY_FILE {
            continue;
        }
        if let Some(file) =
            read_manifest_file(root_canonical, &dir_relative, &relative_path, budget)
        {
            files.push(file);
        }
    }

    Some(SkillRecord {
        scope,
        plugin_name: plugin_name.map(str::to_string),
        name,
        description,
        store: SkillStore::Disk {
            root: root_canonical.to_path_buf(),
            dir_relative,
        },
        frontmatter,
        files,
    })
}

/// 读取一个附件文件并生成 manifest 记录；超预算 / 读取失败返回 `None`（跳过并记录）。
fn read_manifest_file(
    root_canonical: &Path,
    dir_relative: &str,
    relative_path: &str,
    budget: &ResourceBudget,
) -> Option<SkillFileRecord> {
    let full_relative = format!("{dir_relative}/{relative_path}");
    let bytes = match safe_read_file(root_canonical, &full_relative, budget.max_file_bytes) {
        Ok(bytes) => bytes,
        Err(ResourceError::Budget) => {
            tracing::warn!(file = %relative_path, "技能附件超过大小预算，未列入 manifest");
            return None;
        }
        Err(_) => {
            tracing::debug!(file = %relative_path, "技能附件读取失败，未列入 manifest");
            return None;
        }
    };
    Some(SkillFileRecord {
        relative_path: relative_path.to_string(),
        digest: digest_bytes(&bytes),
        size: bytes.len() as u64,
        text: is_text_bytes(&bytes),
    })
}

/// 跨 root 组装 catalog（`skills/list` 与 `resources/list` 的公开批）。
///
/// - 输入 roots 顺序即优先级；跨 root 同名先到先得（shadow 项记 debug 日志）；
/// - builtin 追加在最后（`disable_bundled` 为真时跳过）；
/// - 技能总数超过预算时在稳定的扫描顺序上截断（记 warn）。
pub(crate) fn scan_catalog(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
) -> Vec<SkillRecord> {
    let mut catalog: Vec<SkillRecord> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for root in &input.skill_roots {
        let Some(canonical) = canonical_root(&root.path) else {
            continue;
        };
        for record in scan_root(&canonical, root.scope, root.plugin_name.as_deref(), budget) {
            if !seen.insert(record.name.clone()) {
                tracing::debug!(
                    skill = %record.name,
                    scope = record.scope.as_str(),
                    "同名技能已被更高优先级来源选中，shadow 项不公开"
                );
                continue;
            }
            if catalog.len() >= budget.max_skills {
                tracing::warn!("技能总数超过预算，公开批被截断");
                return catalog;
            }
            catalog.push(record);
        }
    }

    if !input.disable_bundled {
        for record in super::builtin::builtin_skill_records(budget) {
            if !seen.insert(record.name.clone()) {
                continue;
            }
            if catalog.len() >= budget.max_skills {
                tracing::warn!("技能总数超过预算，公开批被截断");
                return catalog;
            }
            catalog.push(record);
        }
    }

    catalog
}

/// 按 URI 定位技能：在 scope（+plugin）匹配的输入根中扫描，取首个 `name` 匹配项
/// （同一根内先到先得；不做跨根去重——shadow 项可读，见模块头）。
pub(crate) fn locate(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
    scope: ResourceScope,
    plugin_name: Option<&str>,
    name: &str,
) -> Result<SkillRecord, ResourceError> {
    if scope == ResourceScope::Builtin {
        if input.disable_bundled {
            // 关闭位（F12）在 provider 侧同样生效：builtin 面不可发现、不可读。
            return Err(ResourceError::InvalidUri);
        }
        return super::builtin::builtin_skill_records(budget)
            .into_iter()
            .find(|record| record.name == name)
            .ok_or(ResourceError::NotFound);
    }
    let mut matched_scope_root = false;
    for root in &input.skill_roots {
        if root.scope != scope || root.plugin_name.as_deref() != plugin_name {
            continue;
        }
        let Some(canonical) = canonical_root(&root.path) else {
            continue;
        };
        matched_scope_root = true;
        if let Some(record) = scan_root(&canonical, root.scope, root.plugin_name.as_deref(), budget)
            .into_iter()
            .find(|record| record.name == name)
        {
            return Ok(record);
        }
    }
    if matched_scope_root {
        Err(ResourceError::NotFound)
    } else {
        // scope/plugin 组合没有配置任何根：与「技能不存在」对外同为找不到，
        // 但内部保留区分（InvalidUri 表示 URI 指向不存在的来源面）。
        Err(ResourceError::InvalidUri)
    }
}

/// `skills/list` 入口（首期不分页：非空 cursor 按非法参数拒绝）。
pub(crate) fn skills_list_response(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
    cursor: Option<String>,
) -> Result<SkillsListResponse, ResourceError> {
    if cursor.is_some() {
        return Err(ResourceError::InvalidUri);
    }
    let catalog = scan_catalog(input, budget);
    let skills = catalog
        .iter()
        .filter_map(SkillRecord::entry)
        .collect::<Vec<_>>();
    Ok(SkillsListResponse {
        skills,
        next_cursor: None,
        ttl_ms: None,
        cache_scope: None,
    })
}

/// `skills/get` 入口：按 URI 返回当前条目快照（不依赖 list 的枚举批）。
pub(crate) fn skills_get_response(
    input: &WorkspaceResourcesInput,
    budget: &ResourceBudget,
    uri: &str,
) -> Result<SkillGetResponse, ResourceError> {
    let parsed = peri_acp_types::workspace_resources::parse_skill_uri(uri)
        .ok_or(ResourceError::InvalidUri)?;
    // `skills/get` 的 URI 是技能入口（规范：条目 uri）；附件 URI 不接受。
    if parsed.relative_path != SKILL_ENTRY_FILE {
        return Err(ResourceError::InvalidUri);
    }
    let record = locate(
        input,
        budget,
        parsed.scope,
        parsed.plugin_name.as_deref(),
        &parsed.name,
    )?;
    let skill = record.entry().ok_or(ResourceError::InvalidUri)?;
    Ok(SkillGetResponse { skill })
}

/// 技能附件 / 入口的 MIME 判定（不按扩展名白名单过滤；未知扩展名按文本性回退）。
pub(crate) fn mime_for(relative_path: &str, text: bool) -> &'static str {
    use peri_acp_types::workspace_resources as contract;
    let extension = Path::new(relative_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    if extension.eq_ignore_ascii_case("md") {
        return contract::MIME_MARKDOWN;
    }
    if extension.eq_ignore_ascii_case("json") {
        return contract::MIME_JSON;
    }
    if text {
        contract::MIME_TEXT
    } else {
        contract::MIME_OCTET_STREAM
    }
}

#[cfg(test)]
#[path = "skills_test.rs"]
mod tests;
