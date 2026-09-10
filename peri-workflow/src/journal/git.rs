//! Read-only Git baseline and write-intent postcondition checks.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone)]
pub struct GitBaseline {
    pub repo_root: PathBuf,
    pub cwd: PathBuf,
    pub head: String,
    pub status_porcelain_v2: Vec<u8>,
}

impl GitBaseline {
    /// 按声明式写入意图捕获 Git baseline。
    ///
    /// `write` 必须位于可验证的 Git 仓库；legacy/read-only 在普通目录仍可执行，
    /// 但没有 baseline 时不能据此宣称 post-processing 或 delivery 已通过。
    pub fn capture_for_intent(
        cwd: &Path,
        intent: Option<&peri_acp_types::workflow::WorkflowWriteIntent>,
    ) -> Result<Option<Self>, String> {
        cwd.canonicalize()
            .map_err(|error| format!("cannot canonicalize workflow cwd: {error}"))?;
        match Self::capture(cwd) {
            Ok(baseline) => {
                if let Some(intent) = intent {
                    baseline.validate_write_intent(intent)?;
                }
                Ok(Some(baseline))
            }
            Err(error)
                if matches!(
                    intent,
                    Some(peri_acp_types::workflow::WorkflowWriteIntent::Write { .. })
                ) =>
            {
                Err(error)
            }
            Err(_) => Ok(None),
        }
    }

    /// 只读捕获 Git baseline。仅使用 plumbing/read-only 命令，不修改 index/worktree。
    pub fn capture(cwd: &Path) -> Result<Self, String> {
        let cwd = cwd
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize workflow cwd: {error}"))?;
        let repo_root = git_output(&cwd, &["rev-parse", "--show-toplevel"])?;
        let repo_root = PathBuf::from(
            String::from_utf8(repo_root)
                .map_err(|_| "git repo root is not UTF-8")?
                .trim(),
        );
        let repo_root = repo_root
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize git repo root: {error}"))?;
        if !cwd.starts_with(&repo_root) {
            return Err("workflow cwd is outside canonical git repository".to_string());
        }
        let head = String::from_utf8(git_output(&cwd, &["rev-parse", "HEAD"])?)
            .map_err(|_| "git HEAD is not UTF-8")?
            .trim()
            .to_string();
        let status_porcelain_v2 =
            git_output(&cwd, &["status", "--porcelain=v2", "--untracked-files=all"])?;
        Ok(Self {
            repo_root,
            cwd,
            head,
            status_porcelain_v2,
        })
    }

    pub fn validate_write_intent(
        &self,
        intent: &peri_acp_types::workflow::WorkflowWriteIntent,
    ) -> Result<(), String> {
        let peri_acp_types::workflow::WorkflowWriteIntent::Write {
            repo_root,
            cwd,
            path_allowlist,
            ..
        } = intent
        else {
            return Ok(());
        };
        let declared_repo = Path::new(repo_root)
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize writeIntent.repo_root: {error}"))?;
        let declared_cwd = Path::new(cwd)
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize writeIntent.cwd: {error}"))?;
        if declared_repo != self.repo_root || declared_cwd != self.cwd {
            return Err(
                "writeIntent repo_root/cwd does not match the active canonical repository"
                    .to_string(),
            );
        }
        if path_allowlist.is_empty() {
            return Err("writeIntent.path_allowlist must not be empty".to_string());
        }
        for declared in path_allowlist {
            let path = Path::new(declared);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err("writeIntent.path_allowlist must contain repository-relative paths without parent traversal".to_string());
            }
            let candidate = self.repo_root.join(path);
            if !candidate.starts_with(&self.repo_root) {
                return Err(
                    "writeIntent.path_allowlist escapes the canonical repository".to_string(),
                );
            }
        }
        Ok(())
    }

    pub fn verify_postcondition(
        &self,
        intent: Option<&peri_acp_types::workflow::WorkflowWriteIntent>,
    ) -> Result<(), String> {
        let Some(peri_acp_types::workflow::WorkflowWriteIntent::Write {
            path_allowlist,
            head_may_change,
            commit_required,
            ..
        }) = intent
        else {
            return self.verify_unchanged();
        };
        let after = Self::capture(&self.cwd)?;
        if after.repo_root != self.repo_root {
            return Err("canonical git repository changed during workflow".to_string());
        }
        let before = parse_status_paths(&self.status_porcelain_v2)?;
        let after_status = parse_status_paths(&after.status_porcelain_v2)?;
        let changed_paths: BTreeSet<String> = before
            .keys()
            .chain(after_status.keys())
            .filter(|path| before.get(*path) != after_status.get(*path))
            .cloned()
            .collect();
        if changed_paths.iter().any(|path| {
            !path_allowlist
                .iter()
                .any(|allowed| path_is_allowed(path, allowed))
        }) {
            return Err("git changes escaped writeIntent.path_allowlist".to_string());
        }
        let head_changed = after.head != self.head;
        if head_changed && !head_may_change {
            return Err("git HEAD changed but writeIntent.head_may_change is false".to_string());
        }
        if commit_required == &Some(true) && !head_changed {
            return Err("writeIntent requires a commit but git HEAD did not change".to_string());
        }
        if head_changed {
            if before
                .values()
                .any(|record| record.starts_with('1') || record.starts_with('2'))
            {
                return Err("cannot attribute a commit while the baseline index already contains staged changes".to_string());
            }
            let committed = git_output(
                &self.cwd,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    &after.head,
                ],
            )?;
            for path in String::from_utf8(committed)
                .map_err(|_| "git commit path list is not UTF-8")?
                .lines()
            {
                if !path_allowlist
                    .iter()
                    .any(|allowed| path_is_allowed(path, allowed))
                {
                    return Err(
                        "git commit contains a path outside writeIntent.path_allowlist".to_string(),
                    );
                }
            }
        }
        Ok(())
    }

    /// 验证运行后没有改写任何 Git 可见事实。失败只报告，不恢复用户内容。
    pub fn verify_unchanged(&self) -> Result<(), String> {
        let after = Self::capture(&self.cwd)?;
        if after.repo_root != self.repo_root {
            return Err("canonical git repository changed during workflow".to_string());
        }
        if after.head != self.head {
            return Err("git HEAD changed without an attributable write postcondition".to_string());
        }
        if after.status_porcelain_v2 != self.status_porcelain_v2 {
            return Err("git index/worktree/untracked facts changed without an attributable write postcondition".to_string());
        }
        Ok(())
    }
}

fn parse_status_paths(status: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let status =
        String::from_utf8(status.to_vec()).map_err(|_| "git status paths are not UTF-8")?;
    let mut records = BTreeMap::new();
    for line in status.lines() {
        let path = match line.as_bytes().first() {
            Some(b'1') => line.splitn(9, ' ').nth(8),
            Some(b'2') => line
                .splitn(10, ' ')
                .nth(9)
                .and_then(|value| value.split('\t').next()),
            Some(b'?') | Some(b'!') => line.get(2..),
            _ => None,
        }
        .ok_or_else(|| "cannot parse git porcelain v2 path".to_string())?;
        records.insert(path.to_string(), line.to_string());
    }
    Ok(records)
}

fn path_is_allowed(path: &str, allowed: &str) -> bool {
    let path = Path::new(path);
    let allowed = Path::new(allowed);
    path == allowed || path.starts_with(allowed)
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to execute git: {error}"))?;
    if !output.status.success() {
        return Err("git pre/postcondition command failed".to_string());
    }
    Ok(output.stdout)
}
