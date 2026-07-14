// [TRAP] 路径穿越防护：validate_and_resolve() 是项目标准入口
// 任何需要接收用户侧相对路径并写入 base_dir 的场景都必须复用此函数。
// 新增类似写入功能时禁止自行实现路径校验。
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use crate::sync::protocol::{FileEntry, SyncItems};

/// 文件写入错误类型
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// 路径穿越攻击或非法路径
    #[error("路径穿越攻击：{0}")]
    PathTraversal(String),
    /// 文件 I/O 错误
    #[error("文件写入失败：{0}")]
    Io(#[from] std::io::Error),
}

/// 规范化路径：消除 . 和 .. 组件，返回纯绝对路径
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => {
                result.push(other);
            }
        }
    }
    result
}

/// 验证相对路径安全并解析为绝对路径
///
/// 安全检查：
/// 1. 拒绝绝对路径（Unix / 开头、Windows C:\ 或 \\ 开头）
/// 2. 拒绝包含 .. 父目录组件的路径（深度计数器检测）
/// 3. 解析后验证最终路径仍以 base_dir 为前缀（兜底防护）
pub fn validate_and_resolve(base_dir: &Path, relative_path: &str) -> Result<PathBuf, WriteError> {
    // Step 1: 拒绝绝对路径
    let rel = Path::new(relative_path);
    if rel.is_absolute()
        || relative_path.starts_with('/')
        || relative_path.starts_with('\\')
        || (relative_path.len() > 2 && relative_path.as_bytes()[1] == b':')
    {
        tracing::warn!("拒绝绝对路径或非法路径前缀: {}", relative_path);
        return Err(WriteError::PathTraversal(format!(
            "绝对路径被拒绝: {}",
            relative_path
        )));
    }

    // Step 2: 逐组件检查 —— 拒绝任何 ParentDir 组件
    let mut depth: i32 = 0;
    for component in rel.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    tracing::warn!("路径穿越攻击拒绝: {} (base: {:?})", relative_path, base_dir);
                    return Err(WriteError::PathTraversal(format!(
                        "路径包含 .. 穿越: {}",
                        relative_path
                    )));
                }
            }
            Component::Normal(_) => depth += 1,
            _ => {} // RootDir 和 Prefix 已在 is_absolute() 中拒绝
        }
    }

    // Step 3: 解析绝对路径并验证仍在 base_dir 内（兜底）
    let resolved = base_dir.join(rel);
    let normalized = normalize_path(&resolved);
    if !normalized.starts_with(base_dir) {
        tracing::warn!(
            "路径解析后逃逸 base_dir: {:?} (base: {:?})",
            normalized,
            base_dir
        );
        return Err(WriteError::PathTraversal(format!(
            "路径逃逸 base_dir: {}",
            relative_path
        )));
    }

    Ok(normalized)
}

/// 向 base_dir 下安全写入一个 FileEntry
///
/// 自动创建父目录，原子写入（写临时文件 → rename）
pub fn write_file_entry(base_dir: &Path, entry: &FileEntry) -> Result<(), WriteError> {
    let target_path = validate_and_resolve(base_dir, &entry.path)?;

    // 确保父目录存在
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // 原子写入：先写临时文件，再 rename
    let tmp_path = target_path.with_extension("tmp");
    fs::write(&tmp_path, &entry.content)?;
    fs::rename(&tmp_path, &target_path)?;

    tracing::info!("已写入文件: {:?}", target_path);
    Ok(())
}

/// 将同步项写入本地文件系统
///
/// 路径映射：
/// - settings → {home_dir}/.peri/settings.json（先备份为 .bak）
/// - skills   → {home_dir}/.claude/skills/{relative_path}
/// - mcp      → {home_dir}/.mcp.json + {cwd}/.mcp.json（如有）
/// - plugins  → {home_dir}/.claude/plugins/cache/{relative_path}
pub fn write_sync_items(home_dir: &Path, cwd: &Path, items: &SyncItems) -> Result<(), WriteError> {
    // 1. 写入 settings.json（原子写入 + 备份）
    if let Some(ref settings) = items.settings {
        let settings_path = home_dir.join(".peri").join("settings.json");
        let bak_path = home_dir.join(".peri").join("settings.json.bak");

        // 备份现有文件（如存在）
        if settings_path.exists() {
            fs::copy(&settings_path, &bak_path)?;
            tracing::info!("已备份 settings.json → settings.json.bak");
        }

        // 确保 .peri 目录存在
        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // 原子写入
        let tmp_path = settings_path.with_extension("tmp");
        fs::write(&tmp_path, settings.content.as_bytes())?;
        fs::rename(&tmp_path, &settings_path)?;

        tracing::info!("已写入 settings.json ({})", settings.content.len());

        // 写入 .claude/settings.json（如有）
        if let Some(ref claude_content) = settings.claude_content {
            let claude_dir = home_dir.join(".claude");
            let claude_path = claude_dir.join("settings.json");

            let bak_path = claude_dir.join("settings.json.bak");
            if claude_path.exists() {
                fs::copy(&claude_path, &bak_path)?;
                tracing::info!("已备份 .claude/settings.json");
            }

            fs::create_dir_all(&claude_dir)?;
            let tmp_path = claude_path.with_extension("tmp");
            fs::write(&tmp_path, claude_content.as_bytes())?;
            fs::rename(&tmp_path, &claude_path)?;

            tracing::info!("已写入 .claude/settings.json ({})", claude_content.len());
        }
    }

    // 2. 写入 skills
    if let Some(ref skills) = items.skills {
        let skills_base = home_dir.join(".claude").join("skills");
        for entry in &skills.files {
            write_file_entry(&skills_base, entry)?;
        }
        tracing::info!("已写入 {} 个 skills 文件", skills.files.len());
    }

    // 3. 写入 MCP 配置
    if let Some(ref mcp) = items.mcp {
        // 全局 .mcp.json
        if let Some(ref global_content) = mcp.global {
            let global_path = home_dir.join(".mcp.json");
            let tmp_path = global_path.with_extension("tmp");
            fs::write(&tmp_path, global_content.as_bytes())?;
            fs::rename(&tmp_path, &global_path)?;
            tracing::info!("已写入全局 .mcp.json");
        }
        // 项目级 .mcp.json
        if let Some(ref project_content) = mcp.project {
            let project_path = cwd.join(".mcp.json");
            let tmp_path = project_path.with_extension("tmp");
            fs::write(&tmp_path, project_content.as_bytes())?;
            fs::rename(&tmp_path, &project_path)?;
            tracing::info!("已写入项目级 .mcp.json");
        }
    }

    // 4. 写入 plugins
    if let Some(ref plugins) = items.plugins {
        let plugins_base = home_dir.join(".claude").join("plugins").join("cache");
        for entry in &plugins.files {
            write_file_entry(&plugins_base, entry)?;
        }
        tracing::info!("已写入 {} 个 plugin 文件", plugins.files.len());
    }

    Ok(())
}
