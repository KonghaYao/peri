//! Hook 执行来源信任（H4）：配置权威在本模块，持久化只经 config MCP 数据面。
//!
//! 语义（用户批准的最小面，不新增时限/ per-run 开关）：
//! - 项目 / 局部 settings hooks **默认不执行**；显式 `grant` 绑定
//!   `canonical workspace + 来源身份 + 来源摘要` 后才放行（非交互默认拒绝）。
//! - 来源身份或摘要变化（例如 settings 文件内容被改动）⇒ 既有授权失效。
//! - `revoke` 立即生效；global 用户级配置不参与（它是用户自己的机器级配置，
//!   且不得被项目 / local / 插件来源借用）。
//! - 信任文件位于选中 global 配置的同一目录（`hook-trust.json`）：项目可写目录
//!   不能自我授权；所有读写经 [`crate::io`]（config MCP 数据面），核心不直接
//!   读盘或绕过数据面做规范化。
//!
//! 规范化 workspace 与摘要都经数据面 / 本模块的确定性哈希计算，调用方只需给出
//! scope 与来源路径。

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 信任文件（与选中 global settings 同目录）。
pub const HOOK_TRUST_FILE: &str = "hook-trust.json";

/// 项目 / 局部 settings 文件相对 workspace 的路径。
pub const PROJECT_SETTINGS_RELATIVE: &str = ".claude/settings.json";
pub const LOCAL_SETTINGS_RELATIVE: &str = ".claude/settings.local.json";

/// settings hooks 的来源种类（global 不参与信任判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSourceKind {
    Project,
    Local,
}

impl SettingsSourceKind {
    pub fn scope(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Local => "local",
        }
    }

    pub fn settings_relative_path(self) -> &'static str {
        match self {
            Self::Project => PROJECT_SETTINGS_RELATIVE,
            Self::Local => LOCAL_SETTINGS_RELATIVE,
        }
    }
}

/// 一条信任记录：workspace + 来源身份 + 来源摘要三者同时匹配才生效。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookTrustEntry {
    /// canonical workspace（经数据面规范化）。
    pub workspace: String,
    /// 归一化来源身份，例如 `project-settings:/abs/path/.claude/settings.json`。
    pub source: String,
    /// 来源摘要（hex sha256，覆盖来源身份与文件内容）。
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HookTrustFile {
    #[serde(default = "trust_version")]
    version: u32,
    #[serde(default)]
    entries: Vec<HookTrustEntry>,
}

fn trust_version() -> u32 {
    1
}

impl Default for HookTrustFile {
    fn default() -> Self {
        Self {
            version: trust_version(),
            entries: Vec::new(),
        }
    }
}

/// 信任文件路径：选中 global settings 的同目录。
pub fn trust_store_path() -> PathBuf {
    let global = crate::io::global_config_path();
    let directory = global
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    directory.join(HOOK_TRUST_FILE)
}

/// 规范化 workspace：路径不存在时返回 `None`（调用方按未信任收口）。
pub fn canonical_workspace(cwd: &Path) -> io::Result<Option<String>> {
    Ok(crate::io::canonicalize(cwd)?.map(|path| path.to_string_lossy().into_owned()))
}

fn normalized_source_path(path: &Path) -> io::Result<String> {
    Ok(match crate::io::canonicalize(path)? {
        Some(canonical) => canonical.to_string_lossy().into_owned(),
        // 文件不存在也要给出稳定身份（此时候选 hooks 为空，但 CLI 需要可诊断输出）。
        None => path.to_string_lossy().into_owned(),
    })
}

/// 确定性摘要：长度前缀编码，避免拼接歧义；覆盖来源身份 + 文件内容。
pub fn content_digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// 为一个 settings 来源计算绑定（workspace / source / digest）。
///
/// 文件不存在时 `digest` 覆盖「缺失」状态；据此授权随后出现的内容不匹配，
/// 需要重新授权。
pub fn settings_binding(
    cwd: &Path,
    kind: SettingsSourceKind,
) -> io::Result<Option<HookTrustEntry>> {
    let Some(workspace) = canonical_workspace(cwd)? else {
        return Ok(None);
    };
    let settings_path = cwd.join(kind.settings_relative_path());
    let source_path = normalized_source_path(&settings_path)?;
    let source = format!("{}-settings:{}", kind.scope(), source_path);
    let content = match crate::io::read_text(&settings_path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let digest = content_digest(&[&workspace, &source, &content]);
    Ok(Some(HookTrustEntry {
        workspace,
        source,
        digest,
    }))
}

fn read_store() -> io::Result<HookTrustFile> {
    match crate::io::read_text(&trust_store_path()) {
        Ok(content) => serde_json::from_str(&content)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HookTrustFile::default()),
        Err(error) => Err(error),
    }
}

fn read_store_raw() -> io::Result<Option<String>> {
    match crate::io::read_text(&trust_store_path()) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn encode_store(store: &HookTrustFile) -> io::Result<String> {
    serde_json::to_string_pretty(store)
        .map(|mut encoded| {
            encoded.push('\n');
            encoded
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// CAS 读-改-写：`mutate` 返回 `false` 表示无需写盘。冲突时重读重试。
fn mutate_store(mut mutate: impl FnMut(&mut HookTrustFile) -> bool) -> io::Result<()> {
    const MAX_ATTEMPTS: usize = 8;
    for _ in 0..MAX_ATTEMPTS {
        let raw = read_store_raw()?;
        let mut store = match &raw {
            Some(content) => serde_json::from_str(content)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            None => HookTrustFile::default(),
        };
        if !mutate(&mut store) {
            return Ok(());
        }
        store.version = trust_version();
        let content = encode_store(&store)?;
        if crate::io::write_text_if_unchanged(&trust_store_path(), &raw, &content)? {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "hook trust store changed concurrently; retry",
    ))
}

/// 该绑定是否已授权（默认拒绝：无记录 / 摘要不符 / workspace 不符都返回 false）。
pub fn is_trusted(entry: &HookTrustEntry) -> io::Result<bool> {
    Ok(read_store()?.entries.iter().any(|known| known == entry))
}

/// 显式授权一条绑定；重复授权时覆盖为最新摘要。
pub fn grant(entry: &HookTrustEntry) -> io::Result<()> {
    mutate_store(|store| {
        if let Some(existing) = store
            .entries
            .iter_mut()
            .find(|known| known.workspace == entry.workspace && known.source == entry.source)
        {
            if *existing == *entry {
                return false;
            }
            *existing = entry.clone();
        } else {
            store.entries.push(entry.clone());
        }
        store.entries.sort_by(|left, right| {
            (&left.workspace, &left.source).cmp(&(&right.workspace, &right.source))
        });
        true
    })
}

/// 撤销：返回是否确有记录被移除。
pub fn revoke(workspace: &str, source: &str) -> io::Result<bool> {
    let mut removed = false;
    mutate_store(|store| {
        let before = store.entries.len();
        store
            .entries
            .retain(|known| !(known.workspace == workspace && known.source == source));
        removed = store.entries.len() != before;
        removed
    })?;
    Ok(removed)
}

/// 列出某 workspace 的授权记录（status / 诊断用）。
pub fn list(workspace: &str) -> io::Result<Vec<HookTrustEntry>> {
    Ok(read_store()?
        .entries
        .into_iter()
        .filter(|known| known.workspace == workspace)
        .collect())
}

#[cfg(test)]
#[path = "trust_test.rs"]
mod tests;
