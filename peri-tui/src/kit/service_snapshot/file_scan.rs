//! 文件资源协调器：按 active execution cwd 扫描浅层文件列表。
//!
//! 这个协调器只负责文件列表的 last-good 缓存和 30 秒节流；cwd 变化会立即使
//! 旧列表失效并触发扫描。它不读取或写入任何会话身份，也不直接发布 atom。

use std::time::{Duration, Instant};

pub(super) struct FileScan {
    pub(super) next: Instant,
    pub(super) cwd: String,
    pub(super) entries: Vec<String>,
}

impl Default for FileScan {
    fn default() -> Self {
        Self {
            next: peri_time::monotonic_now(),
            cwd: String::new(),
            entries: Vec::new(),
        }
    }
}

impl FileScan {
    pub(super) async fn refresh(&mut self, now: Instant, cwd: &str) {
        if now < self.next && cwd == self.cwd {
            return;
        }
        let cwd_for_scan = cwd.to_string();
        self.entries = tokio::task::spawn_blocking(move || scan_cwd_files_shallow(&cwd_for_scan))
            .await
            .unwrap_or_default();
        self.cwd = cwd.to_string();
        self.next = now + Duration::from_secs(30);
    }
}

/// 浅扫 cwd 文件列表（深度=2，最多 500 条），用于 @mention 补全。
///
/// 只扫描顶层文件和一层子目录，并跳过常见的大型或生成目录；失败时返回空列表。
pub(super) fn scan_cwd_files_shallow(cwd: &str) -> Vec<String> {
    use std::fs;
    use std::path::Path;

    const MAX_FILES: usize = 500;
    const IGNORED_DIRS: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".next",
        ".nuxt",
        ".venv",
        "venv",
        ".cache",
        ".idea",
        ".vscode",
        ".DS_Store",
        ".gradle",
        ".maven",
        ".cargo",
        ".rust-analyzer",
        ".ruff_cache",
        ".pytest_cache",
        "coverage",
        "out",
        "bin",
        "obj",
        ".terraform",
        ".serverless",
        ".aws",
        ".kube",
    ];

    let root = Path::new(cwd);
    if !root.exists() || !root.is_dir() {
        return Vec::new();
    }
    let top_entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut subdirs = Vec::new();
    for entry in top_entries.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        let path = entry.path();
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        if IGNORED_DIRS.contains(&name.as_str()) {
            continue;
        }
        if path.is_dir() {
            subdirs.push(path);
        } else {
            push_rel_path(&mut out, &path, root);
        }
    }
    for subdir in subdirs {
        if out.len() >= MAX_FILES {
            break;
        }
        let entries = match fs::read_dir(subdir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => continue,
            };
            if !IGNORED_DIRS.contains(&name.as_str()) && path.is_file() {
                push_rel_path(&mut out, &path, root);
            }
        }
    }
    out
}

fn push_rel_path(out: &mut Vec<String>, path: &std::path::Path, root: &std::path::Path) {
    if let Ok(relative) = path.strip_prefix(root) {
        let value = relative.to_string_lossy().replace('\\', "/");
        if !value.is_empty() {
            out.push(value);
        }
    }
}
