//! 目录扫描器：技能目录发现（leaf 语义）与附件文件枚举。
//!
//! 与既有宿主 loader（`peri-middlewares/src/skills/loader.rs`，W4 迁出前仍在位）
//! 的**有意差异**（W1 契约冻结，X3）：
//! - 不跟随 symlink：扫描发现的 symlink 目录 / 文件一律跳过（loader 是跟随 +
//!   防环）；`SKIP_DIR_NAMES` 名单与 loader 保持一致（减少迁移期的行为漂移），
//!   W4 删除 loader 后成为唯一副本；
//! - 跳过隐藏条目（`.` 开头）：技能公开面不暴露编辑器/工具产物；
//! - 相同输入的产出顺序确定（按文件名字节序排序，与 loader 的 `subdirs.sort()`
//!   同口径）。

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

/// 永远不会包含 SKILL.md 的目录名（与 loader 同名单；跳过以加速扫描）。
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".tox",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "outputs",
    "old_skill",
];

pub(crate) fn should_skip_dir(name: &str) -> bool {
    SKIP_DIR_NAMES.contains(&name)
}

/// 常规文件判定：存在、是普通文件、**不是 symlink**。
pub(crate) fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| !meta.file_type().is_symlink() && meta.is_file())
        .unwrap_or(false)
}

/// 常规目录判定：存在、是目录、**不是 symlink**。
pub(crate) fn is_regular_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| !meta.file_type().is_symlink() && meta.is_dir())
        .unwrap_or(false)
}

/// 在 `root`（canonical 目录）下发现技能目录：含常规 `SKILL.md` 的目录即叶子
/// （不再下钻），否则按排序后的子目录递归（深度与目录数预算）。
///
/// `depth == 0` 的 root 自身不参与隐藏名 / 跳过名单过滤（与 loader 对根目录的
/// 豁免同口径：宿主显式输入的根是授权点）。
pub(crate) fn find_skill_dirs(root: &Path, max_depth: usize, max_dirs: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut dir_count = 0usize;
    walk_for_skills(
        root,
        0,
        max_depth,
        max_dirs,
        &mut visited,
        &mut dir_count,
        &mut found,
    );
    found
}

fn walk_for_skills(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    max_dirs: usize,
    visited: &mut HashSet<PathBuf>,
    dir_count: &mut usize,
    found: &mut Vec<PathBuf>,
) {
    if depth > max_depth || *dir_count >= max_dirs {
        return;
    }
    if !is_regular_dir(dir) {
        return;
    }
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }
    *dir_count += 1;

    if is_regular_file(&dir.join("SKILL.md")) {
        found.push(dir.to_path_buf());
        return;
    }

    let mut subdirs: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if crate::resources::path::is_hidden_name(&name) || should_skip_dir(&name) {
                    return false;
                }
                // symlink（含指向目录的 symlink）不进入发现面。
                entry
                    .file_type()
                    .map(|file_type| file_type.is_dir())
                    .unwrap_or(false)
            })
            .map(|entry| entry.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    subdirs.sort();
    for sub in subdirs {
        walk_for_skills(
            sub.as_path(),
            depth + 1,
            max_depth,
            max_dirs,
            visited,
            dir_count,
            found,
        );
    }
}

/// 枚举 `skill_dir` 下的公开附件文件（相对路径，`/` 分隔，含常规 `SKILL.md`），
/// 按路径排序；跳过隐藏条目、symlink 与跳过名单目录；最多 `max_files` 条。
///
/// 嵌套目录中的其他 `SKILL.md` 在此只作为**附件**出现（发现面已按 leaf 语义
/// 阻止它成为独立技能）。
pub(crate) fn enumerate_skill_files(skill_dir: &Path, max_files: usize) -> Vec<String> {
    let mut files = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    collect_files(skill_dir, skill_dir, &mut files, &mut visited, max_files);
    files.sort();
    files.truncate(max_files);
    files
}

fn collect_files(
    dir: &Path,
    base: &Path,
    out: &mut Vec<String>,
    visited: &mut HashSet<PathBuf>,
    max_files: usize,
) {
    if out.len() >= max_files {
        return;
    }
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if out.len() >= max_files {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if crate::resources::path::is_hidden_name(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_file() {
            if let Some(relative) = relative_path_string(base, &path) {
                out.push(relative);
            }
        } else if file_type.is_dir() {
            if should_skip_dir(&name) {
                continue;
            }
            collect_files(&path, base, out, visited, max_files);
        }
    }
}

/// `base` 相对 `path` 的 `/` 分隔字符串；含非常规分量（`..` 等不会出现）时 `None`。
pub(crate) fn relative_path_string(base: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(base).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

#[cfg(test)]
#[path = "scan_test.rs"]
mod tests;
