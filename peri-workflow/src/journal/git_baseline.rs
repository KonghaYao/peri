//! Read-only Git baseline and write-intent postcondition checks.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostconditionFailure {
    /// 观察到的变化落在本 run 声明写入范围之外：可以报告，但不能归因给本 run。
    Unattributed(String),
    /// 可归因的后置条件违例（越界、HEAD 变化、提交含范围外路径等）。
    Violation(String),
}

impl PostconditionFailure {
    #[must_use]
    pub fn is_violation(&self) -> bool {
        matches!(self, Self::Violation(_))
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Unattributed(message) | Self::Violation(message) => message,
        }
    }
}

impl std::fmt::Display for PostconditionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for PostconditionFailure {}

fn with_paths(message: &str, paths: &BTreeSet<String>) -> String {
    const MAX_PATHS: usize = 20;
    if paths.is_empty() {
        return message.to_string();
    }
    let listed = paths
        .iter()
        .take(MAX_PATHS)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = paths.len().saturating_sub(MAX_PATHS);
    if remaining == 0 {
        format!("{message}; paths: {listed}")
    } else {
        format!("{message}; paths: {listed} (+{remaining} more)")
    }
}

fn unattributed(message: impl Into<String>) -> PostconditionFailure {
    PostconditionFailure::Unattributed(message.into())
}

fn violation(message: impl Into<String>) -> PostconditionFailure {
    PostconditionFailure::Violation(message.into())
}

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
    /// 未声明 intent 时没有可验证的后置条件，不捕获 baseline（也不再对并发变化
    /// 报“不可归因”）：该 run 的 post-processing 只能是 blocked。
    pub fn capture_for_intent(
        cwd: &Path,
        intent: Option<&peri_acp_types::workflow::WorkflowWriteIntent>,
    ) -> Result<Option<Self>, String> {
        cwd.canonicalize()
            .map_err(|error| format!("cannot canonicalize workflow cwd: {error}"))?;
        if intent.is_none() {
            return Ok(None);
        }
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
    ) -> Result<(), PostconditionFailure> {
        let Some(peri_acp_types::workflow::WorkflowWriteIntent::Write {
            path_allowlist,
            head_may_change,
            commit_required,
            ..
        }) = intent
        else {
            return self.verify_unchanged();
        };
        let after = Self::capture(&self.cwd).map_err(|error| {
            unattributed(format!(
                "cannot capture git postcondition baseline: {error}"
            ))
        })?;
        if after.repo_root != self.repo_root {
            return Err(violation(
                "canonical git repository changed during workflow",
            ));
        }
        let before = parse_status_paths(&self.status_porcelain_v2)
            .map_err(|error| unattributed(format!("cannot parse baseline git status: {error}")))?;
        let after_status = parse_status_paths(&after.status_porcelain_v2)
            .map_err(|error| unattributed(format!("cannot parse post-run git status: {error}")))?;
        let changed_paths: BTreeSet<String> = before
            .keys()
            .chain(after_status.keys())
            .filter(|path| before.get(*path) != after_status.get(*path))
            .cloned()
            .collect();
        let escaped: BTreeSet<String> = changed_paths
            .iter()
            .filter(|path| {
                !path_allowlist
                    .iter()
                    .any(|allowed| path_is_allowed(path, allowed))
            })
            .cloned()
            .collect();
        if !escaped.is_empty() {
            return Err(violation(with_paths(
                "git changes escaped writeIntent.path_allowlist",
                &escaped,
            )));
        }
        let head_changed = after.head != self.head;
        if head_changed && !head_may_change {
            return Err(violation(
                "git HEAD changed but writeIntent.head_may_change is false",
            ));
        }
        if commit_required == &Some(true) && !head_changed {
            return Err(violation(
                "writeIntent requires a commit but git HEAD did not change",
            ));
        }
        if head_changed {
            if before
                .values()
                .any(|record| record.starts_with('1') || record.starts_with('2'))
            {
                return Err(violation(
                    "cannot attribute a commit while the baseline index already contains staged changes",
                ));
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
            )
            .map_err(|error| {
                unattributed(format!("cannot inspect the committed path list: {error}"))
            })?;
            let committed_paths = String::from_utf8(committed)
                .map_err(|_| unattributed("git commit path list is not UTF-8"))?;
            let path_items = committed_paths.lines();
            let outside: BTreeSet<String> = path_items
                .filter(|path| !path.is_empty())
                .filter(|path| {
                    !path_allowlist
                        .iter()
                        .any(|allowed| path_is_allowed(path, allowed))
                })
                .map(str::to_string)
                .collect();
            if !outside.is_empty() {
                return Err(violation(with_paths(
                    "git commit contains a path outside writeIntent.path_allowlist",
                    &outside,
                )));
            }
        }
        Ok(())
    }

    /// 验证运行后没有改写任何 Git 可见事实。失败只报告，不恢复用户内容。
    ///
    /// 没有 write footprint 时发生的变化只能报告为未归因（`blocked`），不能当作
    /// 本 run 的后置条件失败；真实的写意图越界由 `verify_postcondition` 判定。
    pub fn verify_unchanged(&self) -> Result<(), PostconditionFailure> {
        let after = Self::capture(&self.cwd).map_err(|error| {
            unattributed(format!(
                "cannot capture git postcondition baseline: {error}"
            ))
        })?;
        if after.repo_root != self.repo_root {
            return Err(unattributed(
                "canonical git repository changed outside the run's write footprint",
            ));
        }
        if after.head != self.head {
            return Err(unattributed(
                "git HEAD changed outside the run's write footprint; attribution unavailable",
            ));
        }
        if after.status_porcelain_v2 != self.status_porcelain_v2 {
            let before = parse_status_paths(&self.status_porcelain_v2).map_err(|error| {
                unattributed(format!("cannot parse baseline git status: {error}"))
            })?;
            let after_status = parse_status_paths(&after.status_porcelain_v2).map_err(|error| {
                unattributed(format!("cannot parse post-run git status: {error}"))
            })?;
            let changed_paths: BTreeSet<String> = before
                .keys()
                .chain(after_status.keys())
                .filter(|path| before.get(*path) != after_status.get(*path))
                .cloned()
                .collect();
            return Err(unattributed(with_paths(
                "git index/worktree/untracked facts changed outside the run's write footprint; attribution unavailable",
                &changed_paths,
            )));
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
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    let output = peri_process::run_output_blocking(command)
        .map_err(|error| format!("failed to execute git: {error}"))?;
    #[cfg(not(unix))]
    let output = command
        .output()
        .map_err(|error| format!("failed to execute git: {error}"))?;
    if !output.status.success() {
        return Err("git pre/postcondition command failed".to_string());
    }
    Ok(output.stdout)
}
