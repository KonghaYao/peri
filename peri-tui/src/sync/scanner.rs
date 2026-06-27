// [TRAP] Path::strip_prefix() + to_string_lossy() 在 Windows 上产生 \ 分隔符
// sync 协议要求 /。构造 FileEntry.path 时必须 .replace('\\', "/")。
// 详见 spec/global/domains/sync.md#issue_2026-05-20-windows-path-separator-breaks-tests
use std::{fs, path::Path};

use crate::sync::protocol::{FileEntry, FilesItem, McpItem, SettingsItem, SyncItems, SyncPackage};

/// 递归扫描目录，返回相对路径的文件列表
fn scan_directory(base: &Path) -> Vec<FileEntry> {
    let mut files = Vec::new();
    if !base.exists() || !base.is_dir() {
        return files;
    }
    scan_dir_recursive(base, base, &mut files);
    files
}

fn scan_dir_recursive(base: &Path, dir: &Path, files: &mut Vec<FileEntry>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("无法读取目录 {:?}: {}", dir, e);
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let rel = match path.strip_prefix(base) {
                Ok(r) => r,
                Err(_) => continue,
            };
            match fs::read(&path) {
                Ok(content) => files.push(FileEntry {
                    // Normalize to forward slashes for cross-platform compatibility
                    path: rel.to_string_lossy().replace('\\', "/"),
                    content,
                }),
                Err(e) => tracing::warn!("无法读取文件 {:?}: {}", path, e),
            }
        } else if path.is_dir() {
            scan_dir_recursive(base, &path, files);
        }
    }
}

/// 扫描 settings.json
///
/// 路径：`{home_dir}/.peri/settings.json` + `{home_dir}/.claude/settings.json`
pub fn scan_settings(home_dir: &Path) -> Option<SettingsItem> {
    let path = home_dir.join(".peri").join("settings.json");
    let claude_path = home_dir.join(".claude").join("settings.json");

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("无法读取 settings.json {:?}: {}", path, e);
            return None;
        }
    };
    let claude_content = fs::read_to_string(&claude_path).ok();

    Some(SettingsItem {
        content,
        claude_content,
    })
}

/// 扫描 skills 目录
///
/// 路径：`{home_dir}/.claude/skills/`
pub fn scan_skills(home_dir: &Path) -> FilesItem {
    let base = home_dir.join(".claude").join("skills");
    let files = scan_directory(&base);
    tracing::info!("从 skills 目录扫描到 {} 个文件", files.len());
    FilesItem { files }
}

/// 扫描 MCP 配置
///
/// 全局：`{home_dir}/.mcp.json`，项目级：`{cwd}/.mcp.json`
pub fn scan_mcp(home_dir: &Path, cwd: &Path) -> McpItem {
    let global_path = home_dir.join(".mcp.json");
    let project_path = cwd.join(".mcp.json");
    let global = fs::read_to_string(&global_path).ok();
    let project = fs::read_to_string(&project_path).ok();
    McpItem { global, project }
}

/// 扫描已安装插件
///
/// 路径：`{home_dir}/.claude/plugins/cache/`
pub fn scan_plugins(home_dir: &Path) -> FilesItem {
    let base = home_dir.join(".claude").join("plugins").join("cache");
    let files = scan_directory(&base);
    tracing::info!("从 plugins 缓存扫描到 {} 个文件", files.len());
    FilesItem { files }
}

/// 按需扫描所有本地配置，构建 SyncPackage
///
/// `items_filter` 由 receiver 传回，字段为 `Some` 表示需要同步该类别。
pub fn scan_all(home_dir: &Path, cwd: &Path, items_filter: &SyncItems) -> SyncPackage {
    use std::time::{SystemTime, UNIX_EPOCH};

    let items = SyncItems {
        settings: if items_filter.settings.is_some() {
            scan_settings(home_dir)
        } else {
            None
        },
        skills: items_filter.skills.as_ref().map(|_| scan_skills(home_dir)),
        mcp: items_filter.mcp.as_ref().map(|_| scan_mcp(home_dir, cwd)),
        plugins: items_filter
            .plugins
            .as_ref()
            .map(|_| scan_plugins(home_dir)),
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    SyncPackage {
        version: 1,
        timestamp,
        items,
    }
}
