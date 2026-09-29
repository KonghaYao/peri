//! MetaHarness 段落覆盖文档资源面（`peri-meta://workspace/{section_id}`，J6/W3b）。
//!
//! 语义口径（W1 契约冻结 + 宿主旧 scanner 逐字保留，plan §6.4）：
//! - 扫描 `{cwd}/.peri/meta` 的**一级** `*.md` 文件（不递归；目录即使名为
//!   `x.md` 也忽略，与旧 scanner 的 `is_file()` 口径一致）；文件名 stem =
//!   `section_id`，`read` 返回**全文字节语义**（不 trim、不解析 frontmatter）；
//! - 列出**全部**扫描到的 stem（含不在 `SECTION_IDS` 中的文件）；provider 不
//!   解释 `section_id`、不判断配置开关——是否消费由宿主冻结期按启用 section 决定；
//! - 文件名非 UTF-8、正文非 UTF-8、条目/文件不可读 → 跳过并 warn；目录不存在或
//!   不可读 → 空集合（**不是错误**；X8 要求 list 空与 read 错误可区分）；
//! - `read` 只服务扫描得到的 stem（**不做任意路径 join**，无遍历面）：每次请求
//!   实时扫描，命中即返回该次扫描读到的正文；未知 / 不可得 → `NotFound`
//!   （handler 映射 `-32602` / `-32002`，见 `workspace.rs`）。
//!
//! 与宿主旧 scanner（`peri-middlewares/src/meta_harness`）的**有意差异**（W1
//! 冻结的公开面不变量，随 scanner 删除由本模块承担）：
//! - **禁 symlink**：`.peri/meta` 下的 symlink 条目跳过并记日志（旧 scanner 的
//!   `path.is_file()` 会跟随 symlink 读到工作区外的文件；公开面禁 symlink 是
//!   X3/W1 对技能、agent 定义同款的口径）；
//! - **不施加大小预算**：旧 scanner 无大小上限，加预算会静默丢失大文档；
//! - 产出顺序确定（按 `section_id` 字节序排序；旧 scanner 是无序 map）。

use std::path::Path;

use peri_acp_types::workspace_resources::{digest_bytes, meta_uri};

use super::ResourceError;

/// 段落覆盖目录（相对工作区绑定根 cwd；`.peri/meta/` 的位置与命名约定不改）。
pub(crate) const META_DIR_RELATIVE: &str = ".peri/meta";

/// 已扫描的段落覆盖文档。
#[derive(Debug, Clone)]
pub(crate) struct MetaSection {
    /// 段落 ID（= 文件 stem 的逐字值）。
    pub section_id: String,
    /// 全文（原始字节的 UTF-8 视图；不 trim、不解析 frontmatter）。
    pub text: String,
    /// 原始字节 digest（读取面复算一致）。
    pub digest: String,
}

/// 一次读取请求的产物（与 `skills`/`instructions` 的读取投影同构）。
#[derive(Debug)]
pub(crate) struct MetaRead {
    pub text: String,
    pub digest: String,
}

/// 扫描 `{cwd}/.peri/meta/*.md`（一级、非递归、按 `section_id` 排序）。
pub(crate) fn scan_sections(cwd: &Path) -> Vec<MetaSection> {
    let dir = cwd.join(META_DIR_RELATIVE);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            // 目录不存在是常态（无覆盖的工作区）；不可读与不存在都不 fail 扫描，
            // 「覆盖不可得」的可观察性由宿主按启用 section 读不到时告警（X8）。
            tracing::debug!(
                dir = META_DIR_RELATIVE,
                %error,
                "MetaHarness 覆盖目录不可读，按无段落处理"
            );
            return Vec::new();
        }
    };
    let mut sections = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "MetaHarness 覆盖目录条目不可读，跳过");
                continue;
            }
        };
        if let Some(section) = scan_entry(&entry) {
            sections.push(section);
        }
    }
    sections.sort_by_key(|section| section.section_id.clone());
    sections
}

/// 读取指定段落（实时扫描；只服务扫描得到的 stem）。
pub(crate) fn read(cwd: &Path, section_id: &str) -> Result<MetaRead, ResourceError> {
    scan_sections(cwd)
        .into_iter()
        .find(|section| section.section_id == section_id)
        .map(|section| MetaRead {
            text: section.text,
            digest: section.digest,
        })
        .ok_or(ResourceError::NotFound)
}

/// 段落 ID = 文件 stem；非 UTF-8 文件名返回 `None`（旧 scanner 的
/// `file_stem().to_str()` 口径，调用方 warn + 跳过）。
fn section_id_of(path: &Path) -> Option<&str> {
    path.file_stem().and_then(|stem| stem.to_str())
}

/// 单个目录条目 → 段落（不满足公开条件时记日志并跳过）。
fn scan_entry(entry: &std::fs::DirEntry) -> Option<MetaSection> {
    let path = entry.path();
    let name = entry.file_name();
    let label = name.to_string_lossy();
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(error) => {
            tracing::warn!(entry = %label, %error, "MetaHarness 覆盖条目类型不可读，跳过");
            return None;
        }
    };
    if file_type.is_symlink() {
        tracing::debug!(entry = %label, "MetaHarness 覆盖条目是 symlink，不在公开面，跳过");
        return None;
    }
    if !file_type.is_file() {
        // 目录（即使名为 `x.md`）与特殊文件忽略，不递归。
        return None;
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        return None;
    }
    let Some(section_id) = section_id_of(&path) else {
        tracing::warn!(entry = %label, "MetaHarness 覆盖文档文件名非 UTF-8，跳过");
        return None;
    };
    if meta_uri(section_id).is_none() {
        tracing::warn!(
            section = %section_id,
            "MetaHarness 段落 ID 不符合 URI 段约束，不公开"
        );
        return None;
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(section = %section_id, %error, "MetaHarness 覆盖文档不可读，跳过");
            return None;
        }
    };
    let Ok(text) = String::from_utf8(bytes) else {
        tracing::warn!(section = %section_id, "MetaHarness 覆盖文档非 UTF-8 文本，跳过");
        return None;
    };
    let digest = digest_bytes(text.as_bytes());
    Some(MetaSection {
        section_id: section_id.to_string(),
        text,
        digest,
    })
}

#[cfg(test)]
#[path = "meta_test.rs"]
mod tests;
