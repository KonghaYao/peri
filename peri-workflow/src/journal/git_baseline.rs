//! Read-only Git baseline and write-intent postcondition checks.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostconditionFailure {
    /// 无法观测（采样或解析失败、验证范围不可用）：不得据此宣称“无变化”。
    ObservationIncomplete(String),
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

    /// 交付投影的结构化原因；可归因违例由 `PostProcessingStatus::Failed` 表达。
    #[must_use]
    pub fn delivery_reason(&self) -> Option<peri_acp_types::workflow::DeliveryReason> {
        match self {
            Self::ObservationIncomplete(_) => {
                Some(peri_acp_types::workflow::DeliveryReason::ObservationIncomplete)
            }
            Self::Unattributed(_) => {
                Some(peri_acp_types::workflow::DeliveryReason::UnattributedChange)
            }
            Self::Violation(_) => None,
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::ObservationIncomplete(message)
            | Self::Unattributed(message)
            | Self::Violation(message) => message,
        }
    }

    /// 归因档位：写进终态错误文案，避免把外部变化读成本 run 写入。
    #[must_use]
    pub fn attribution_label(&self) -> &'static str {
        match self {
            Self::Violation(_) => "run-owned violation",
            Self::Unattributed(_) => "unattributed change (external or unknown)",
            Self::ObservationIncomplete(_) => "observation incomplete",
        }
    }
}

impl std::fmt::Display for PostconditionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {}",
            self.attribution_label(),
            self.message()
        )
    }
}

impl std::error::Error for PostconditionFailure {}

fn with_paths(message: &str, paths: &BTreeSet<Vec<u8>>) -> String {
    const MAX_PATHS: usize = 20;
    if paths.is_empty() {
        return message.to_string();
    }
    // 仅用于文案：路径本身以字节参与比较，展示时有损解码不影响判定。
    let listed = paths
        .iter()
        .take(MAX_PATHS)
        .map(|path| String::from_utf8_lossy(path).into_owned())
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

fn observation_incomplete(message: impl Into<String>) -> PostconditionFailure {
    PostconditionFailure::ObservationIncomplete(message.into())
}

fn violation(message: impl Into<String>) -> PostconditionFailure {
    PostconditionFailure::Violation(message.into())
}

/// 启动时捕获的 Git 观测窗口。
///
/// [`Self::Incomplete`] 与 [`Self::NotApplicable`] 都不是“没有变化”：前者无法观测，
/// 后者没有可绑定的仓库范围；交付投影不得据此显示通过。
#[derive(Debug, Clone)]
pub enum GitObservation {
    /// 基线已采样，运行后可以对比。
    Captured(GitBaseline),
    /// 非 Git 仓库：没有可绑定的仓库范围。
    NotApplicable,
    /// 采样失败；保留原因用于诊断。
    Incomplete(String),
}

impl Default for GitObservation {
    fn default() -> Self {
        Self::Incomplete("no git observation was captured".to_string())
    }
}

impl GitObservation {
    #[must_use]
    pub fn completeness(&self) -> peri_acp_types::workflow::PostconditionObservation {
        use peri_acp_types::workflow::PostconditionObservation;
        match self {
            Self::Captured(_) => PostconditionObservation::Complete,
            Self::NotApplicable => PostconditionObservation::NotApplicable,
            Self::Incomplete(_) => PostconditionObservation::Incomplete,
        }
    }

    /// 观测身份与能力；只有成功采样才有可持久化的范围。
    #[must_use]
    pub fn scope(&self) -> Option<peri_acp_types::workflow::GitObservationScope> {
        match self {
            Self::Captured(baseline) => Some(baseline.observation_scope()),
            Self::NotApplicable | Self::Incomplete(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitBaseline {
    pub repo_root: PathBuf,
    pub cwd: PathBuf,
    pub git_dir: PathBuf,
    pub git_common_dir: PathBuf,
    pub head: String,
    pub status_porcelain_v2: Vec<u8>,
}

impl GitBaseline {
    /// 按声明式写入意图捕获 Git baseline。
    ///
    /// `write` 必须位于可验证的 Git 仓库，否则启动失败（否则无法验证写入边界）。
    /// legacy/read-only 在普通目录仍可执行：采样失败返回
    /// [`GitObservation::Incomplete`]，非 Git 目录返回 [`GitObservation::NotApplicable`]，
    /// 都不阻断 start，但也不能据此宣称 post-processing 或 delivery 已通过。
    /// 未声明 intent 时同样采样：没有意图不等于“没有变化”，只表示未请求交付。
    pub fn capture_for_intent(
        cwd: &Path,
        intent: Option<&peri_acp_types::workflow::WorkflowWriteIntent>,
    ) -> Result<GitObservation, String> {
        // cwd 本身不可用属于启动输入错误，不是“Git 采样失败”。
        let cwd = cwd
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize workflow cwd: {error}"))?;
        if let Some(intent) = intent {
            if matches!(
                intent,
                peri_acp_types::workflow::WorkflowWriteIntent::Write { .. }
            ) {
                let baseline = Self::capture(&cwd)?;
                baseline.validate_write_intent(intent)?;
                return Ok(GitObservation::Captured(baseline));
            }
        }
        Ok(match Self::capture(&cwd) {
            Ok(baseline) => GitObservation::Captured(baseline),
            // 用规范化路径探测 `.git` marker：符号链接与真实路径必须是同一结论。
            Err(error) => classify_unobserved(&cwd, error),
        })
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
        let git_dir = capture_git_path(&cwd, "--git-dir")?;
        let git_common_dir = capture_git_path(&cwd, "--git-common-dir")?;
        let status_porcelain_v2 = git_output(
            &cwd,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        )?;
        Ok(Self {
            repo_root,
            cwd,
            git_dir,
            git_common_dir,
            head,
            status_porcelain_v2,
        })
    }

    /// 观测身份与能力：终态只持久化这些，不保存基线内容。
    #[must_use]
    pub fn observation_scope(&self) -> peri_acp_types::workflow::GitObservationScope {
        use peri_acp_types::workflow::{GitObservationScope, GitObservationVisibility};
        GitObservationScope {
            repo_root: self.repo_root.to_string_lossy().into_owned(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            git_dir: self.git_dir.to_string_lossy().into_owned(),
            git_common_dir: self.git_common_dir.to_string_lossy().into_owned(),
            head: self.head.clone(),
            visibility: GitObservationVisibility {
                index: true,
                worktree: true,
                ignored: false,
                commit_range: false,
            },
        }
    }

    /// Git 目录身份变化意味着观测窗口已切换（如 worktree 重新绑定），
    /// 不能拿旧基线比较后宣称“无变化”。
    fn check_observation_window(&self, after: &Self) -> Result<(), PostconditionFailure> {
        if after.git_dir != self.git_dir || after.git_common_dir != self.git_common_dir {
            return Err(observation_incomplete(
                "git dir/common dir changed since the baseline; the observation window no longer matches",
            ));
        }
        Ok(())
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

    /// workflow 自身 journal 目录（相对 repo root）。
    ///
    /// 该目录由宿主在运行期间写入；未把它加入 `.gitignore` 的仓库不能因此把本 run
    /// 自己的产物判成外部变化。
    fn workflow_owned_prefix(&self) -> Option<PathBuf> {
        self.cwd
            .join(super::workflow_journal_store::WORKFLOW_RUNS_DIR)
            .strip_prefix(&self.repo_root)
            .ok()
            .map(Path::to_path_buf)
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
            observation_incomplete(format!(
                "cannot capture git postcondition baseline: {error}"
            ))
        })?;
        if after.repo_root != self.repo_root {
            return Err(violation(
                "canonical git repository changed during workflow",
            ));
        }
        self.check_observation_window(&after)?;
        let before = parse_status_paths(&self.status_porcelain_v2).map_err(|error| {
            observation_incomplete(format!("cannot parse baseline git status: {error}"))
        })?;
        let after_status = parse_status_paths(&after.status_porcelain_v2).map_err(|error| {
            observation_incomplete(format!("cannot parse post-run git status: {error}"))
        })?;
        let owned = self.workflow_owned_prefix();
        let before = strip_workflow_owned(before, owned.as_deref());
        let after_status = strip_workflow_owned(after_status, owned.as_deref());
        let changed_paths: BTreeSet<Vec<u8>> = before
            .keys()
            .chain(after_status.keys())
            .filter(|path| before.get(*path) != after_status.get(*path))
            .cloned()
            .collect();
        let escaped: BTreeSet<Vec<u8>> = changed_paths
            .iter()
            .filter(|path| {
                !path_allowlist
                    .iter()
                    .any(|allowed| path_is_allowed(path.as_slice(), allowed))
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
                .any(|record| matches!(record.first(), Some(b'1' | b'2')))
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
                observation_incomplete(format!("cannot inspect the committed path list: {error}"))
            })?;
            let committed_paths = String::from_utf8(committed)
                .map_err(|_| observation_incomplete("git commit path list is not UTF-8"))?;
            let path_items = committed_paths.lines();
            let outside: BTreeSet<Vec<u8>> = path_items
                .filter(|path| !path.is_empty())
                .filter(|path| {
                    !path_allowlist
                        .iter()
                        .any(|allowed| path_is_allowed(path.as_bytes(), allowed))
                })
                .map(|path| path.as_bytes().to_vec())
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
            observation_incomplete(format!(
                "cannot capture git postcondition baseline: {error}"
            ))
        })?;
        if after.repo_root != self.repo_root {
            return Err(unattributed(
                "canonical git repository changed outside the run's write footprint",
            ));
        }
        self.check_observation_window(&after)?;
        if after.head != self.head {
            return Err(unattributed(
                "git HEAD changed outside the run's write footprint; attribution unavailable",
            ));
        }
        let before = parse_status_paths(&self.status_porcelain_v2).map_err(|error| {
            observation_incomplete(format!("cannot parse baseline git status: {error}"))
        })?;
        let after_status = parse_status_paths(&after.status_porcelain_v2).map_err(|error| {
            observation_incomplete(format!("cannot parse post-run git status: {error}"))
        })?;
        // workflow 自身 journal 目录不计入对账：未忽略 `.claude` 的仓库不能把
        // 本 run 刚写下的运行目录当成外部变化。
        let owned = self.workflow_owned_prefix();
        let before = strip_workflow_owned(before, owned.as_deref());
        let after_status = strip_workflow_owned(after_status, owned.as_deref());
        if before != after_status {
            let changed_paths: BTreeSet<Vec<u8>> = before
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

/// 解析 `git status --porcelain=v2 -z` 的路径记录。
///
/// `-z` 让 git 不再 C-quote 路径（非 ASCII 目录也能与 workflow-owned 前缀比较）；
/// rename/copy 的原始路径是紧随其后的独立 NUL 字段，两个端点都要参与对账，
/// 否则“从仓库外 rename 进 journal”会漏掉源路径的消失。
pub(super) fn parse_status_paths(status: &[u8]) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, String> {
    // 键必须是路径原始字节：有损解码可能让两个不同的非 UTF-8 路径折叠成同一个键，
    // 从而把真实变化读成“无变化”（false green）。
    let mut records = BTreeMap::new();
    let mut fields = status
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        let (path, has_rename_source) = match field.first() {
            Some(b'1') => (field.splitn(9, |byte| *byte == b' ').nth(8), false),
            Some(b'2') => (field.splitn(10, |byte| *byte == b' ').nth(9), true),
            Some(b'u') => (field.splitn(11, |byte| *byte == b' ').nth(10), false),
            Some(b'?') | Some(b'!') => (field.get(2..), false),
            _ => (None, false),
        };
        let path = path.ok_or_else(|| "cannot parse git porcelain v2 path".to_string())?;
        records.insert(path.to_vec(), field.to_vec());
        if has_rename_source {
            let source = fields
                .next()
                .ok_or_else(|| "cannot parse git porcelain v2 rename source".to_string())?;
            records.insert(source.to_vec(), field.to_vec());
        }
    }
    Ok(records)
}

/// 把 porcelain 路径字节还原为 `Path`：Unix 保留原始字节，非 UTF-8 路径也能精确比较；
/// 其他平台没有字节级 `Path`，退回有损解码，只影响前缀/allowlist 判定，键比较仍是字节。
fn status_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// 移除 workflow 自身 journal 目录下的记录；其余路径保持不变。
fn strip_workflow_owned(
    records: BTreeMap<Vec<u8>, Vec<u8>>,
    owned: Option<&Path>,
) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let Some(owned) = owned else {
        return records;
    };
    records
        .into_iter()
        .filter(|(path, _)| !status_path(path).starts_with(owned))
        .collect()
}

fn path_is_allowed(path: &[u8], allowed: &str) -> bool {
    let path = status_path(path);
    let allowed = Path::new(allowed);
    path.as_path() == allowed || path.starts_with(allowed)
}

/// 采样失败时区分“明确不在 Git 仓库”与“无法观测”。
///
/// 只有 git 可执行、探测确认不在 worktree 且没有 `.git` marker 时才判
/// [`GitObservation::NotApplicable`]；损坏的仓库或缺 git 都按采样失败处理，
/// 否则会把“读不到”误报成“没有可观测的变化”。
fn classify_unobserved(cwd: &Path, error: String) -> GitObservation {
    match git_worktree_probe(cwd) {
        Ok(true) => GitObservation::Incomplete(error),
        Ok(false) if has_git_marker(cwd) => GitObservation::Incomplete(error),
        Ok(false) => GitObservation::NotApplicable,
        Err(_) => GitObservation::Incomplete(error),
    }
}

fn git_worktree_probe(cwd: &Path) -> Result<bool, String> {
    let output = run_git(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if !output.status.success() {
        return Ok(false);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn has_git_marker(cwd: &Path) -> bool {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return true;
        }
        current = dir.parent();
    }
    false
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
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
    Ok(output)
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = run_git(cwd, args)?;
    if !output.status.success() {
        // 只保留 git 自身的 stderr 摘要（不拼接环境变量），否则统一文本会丢失诊断。
        const STDERR_LIMIT: usize = 500;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            "no stderr output".to_string()
        } else {
            peri_acp_types::session::bounded_error_message(stderr, STDERR_LIMIT)
        };
        return Err(format!("git {} failed: {detail}", args.join(" ")));
    }
    Ok(output.stdout)
}

/// 解析 `rev-parse --git-dir` / `--git-common-dir`：相对路径以 cwd 为基准，
/// canonicalize 后作为跨采样可比较的观测身份。
fn capture_git_path(cwd: &Path, flag: &str) -> Result<PathBuf, String> {
    let raw = String::from_utf8(git_output(cwd, &["rev-parse", flag])?)
        .map_err(|_| format!("git {flag} is not UTF-8"))?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("git {flag} is empty"));
    }
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    absolute
        .canonicalize()
        .map_err(|error| format!("cannot canonicalize git {flag}: {error}"))
}
