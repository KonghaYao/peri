use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::sync::protocol::{FileEntry, SyncPackage};
use crate::sync::writer;

// ─── staging / commit（TRAP 路径校验复用 writer）───────────────────────────

/// staging 目录：`~/.peri/staging-<channel_hash>`（home 同卷，M2：保证 commit
/// rename 不跨卷；channel_hash 为日志用 8-hex 表示）。
pub(crate) fn staging_dir_for(home: &Path, channel_hash: &str) -> PathBuf {
    home.join(".peri").join(format!("staging-{channel_hash}"))
}

/// 创建 staging 目录（unix 下 0700）。残留同名目录（上次崩溃遗留，内容从未
/// commit）先整体清理，避免混入本次暂存。父目录（`~/.peri`）递归创建。
pub(crate) fn create_staging_dir(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("cannot clear stale staging dir {}", path.display()))?;
    }
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path)
    };
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().recursive(true).create(path);
    created.with_context(|| format!("cannot create staging dir {}", path.display()))?;
    Ok(())
}

/// 暂存文件：staging 内路径 → 目标路径（commit 时原子 rename）。
#[derive(Debug)]
pub(crate) struct StagedFile {
    pub(crate) staged: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) backup: Option<PathBuf>,
}

/// 把 SyncPackage 全量写入 staging 目录（保持 writer 的语义与 TRAP 校验）。
/// 任一项失败即整体失败：不 commit、不 confirm。
pub(crate) fn stage_package(
    home: &Path,
    cwd: &Path,
    pkg: &SyncPackage,
    staging: &Path,
) -> Result<Vec<StagedFile>> {
    let mut out = Vec::new();
    if let Some(settings) = &pkg.items.settings {
        let staged = staging.join("settings.json");
        write_staged(&staged, settings.content.as_bytes())?;
        out.push(StagedFile {
            staged,
            target: home.join(".peri").join("settings.json"),
            backup: Some(home.join(".peri").join("settings.json.bak")),
        });
        if let Some(claude) = &settings.claude_content {
            let staged = staging.join("claude-settings.json");
            write_staged(&staged, claude.as_bytes())?;
            out.push(StagedFile {
                staged,
                target: home.join(".claude").join("settings.json"),
                backup: Some(home.join(".claude").join("settings.json.bak")),
            });
        }
    }
    if let Some(skills) = &pkg.items.skills {
        let skills_base = home.join(".claude").join("skills");
        for entry in &skills.files {
            out.push(stage_file(staging, &skills_base, entry, "skills")?);
        }
    }
    if let Some(mcp) = &pkg.items.mcp {
        if let Some(global) = &mcp.global {
            let staged = staging.join("mcp-global.json");
            write_staged(&staged, global.as_bytes())?;
            out.push(StagedFile {
                staged,
                target: home.join(".mcp.json"),
                backup: None,
            });
        }
        if let Some(project) = &mcp.project {
            let staged = staging.join("mcp-project.json");
            write_staged(&staged, project.as_bytes())?;
            out.push(StagedFile {
                staged,
                target: cwd.join(".mcp.json"),
                backup: None,
            });
        }
    }
    if let Some(plugins) = &pkg.items.plugins {
        let plugins_base = home.join(".claude").join("plugins").join("cache");
        for entry in &plugins.files {
            out.push(stage_file(staging, &plugins_base, entry, "plugins")?);
        }
    }
    Ok(out)
}

/// 单个文件入 staging：TRAP 路径校验针对真实目标 base（`writer::validate_and_resolve`）。
fn stage_file(
    staging: &Path,
    target_base: &Path,
    entry: &FileEntry,
    kind: &str,
) -> Result<StagedFile> {
    let target = writer::validate_and_resolve(target_base, &entry.path)
        .map_err(|_| anyhow::anyhow!("package contains an unsafe path"))?;
    let staged = staging.join(kind).join(&entry.path);
    write_staged(&staged, &entry.content)?;
    Ok(StagedFile {
        staged,
        target,
        backup: None,
    })
}

/// 写暂存文件（unix 下 0600；父目录 0700，M2）。
fn write_staged(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dirs(parent)?;
    }
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    };
    let mut file =
        opened.with_context(|| format!("cannot write staged file {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

/// 创建暂存目录树；最深层目录在 unix 下收紧为 0700（staging 根已 0700，
/// 中间层在 0700 根内，外部不可达）。
fn create_private_dirs(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// 把 staging 文件逐个原子移动到目标路径；settings 语义保持 `.bak` 备份。
/// 中途失败不回滚已完成的文件（plan：失败不回滚已提交文件）。
pub(crate) fn commit_files(writes: &[StagedFile]) -> Result<()> {
    commit_files_with(writes, |from, to| std::fs::rename(from, to))
}

/// 同 [`commit_files`]，但移动操作可注入（测试注入 rename 失败验证回退）。
///
/// M2 复审修复：rename 失败（如跨卷 EXDEV 或目标被占用）回退 copy + 清理
/// staged 文件——staging 与目标同卷（home 下）后 rename 正常路径不跨卷。
pub(crate) fn commit_files_with(
    writes: &[StagedFile],
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    for w in writes {
        if let Some(parent) = w.target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(backup) = &w.backup
            && w.target.exists()
        {
            std::fs::copy(&w.target, backup)?;
        }
        match rename(&w.staged, &w.target) {
            Ok(()) => {}
            Err(_) => {
                // 回退路径：copy 非原子，仅用于 rename 不可用的情况。
                std::fs::copy(&w.staged, &w.target).with_context(|| {
                    format!(
                        "rename and copy fallback both failed for {}",
                        w.target.display()
                    )
                })?;
                let _ = std::fs::remove_file(&w.staged);
            }
        }
    }
    Ok(())
}
