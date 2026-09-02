//! 磁盘持久化：workflow 运行日志、状态快照、脚本副本。
//!
//! 目录结构：`.claude/workflow-runs/<runId>/`
//! - `journal.jsonl` — append-only agent() 调用结果日志（用于 cache-hit resume）
//! - `state.json` — 最终状态快照（run_done 时原子写入）
//! - `script.js` — workflow 脚本源码副本

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use peri_acp_types::workflow::{
    AcceptanceStatus, DeliveryStatus, ExecutionStatus, PostProcessingStatus,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::protocol::JournalEntry;

const WORKFLOW_RUNS_DIR: &str = ".claude/workflow-runs";
const KEEP_MAX_RUNS: usize = 50;

fn limits_are_empty(limits: &crate::protocol::WorkflowLimits) -> bool {
    limits.max_agents.is_none()
        && limits.max_tool_calls.is_none()
        && limits.max_elapsed_ms.is_none()
}

/// workflow 运行的持久化状态快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    #[serde(default)]
    pub execution_status: ExecutionStatus,
    #[serde(default)]
    pub acceptance_status: AcceptanceStatus,
    #[serde(default)]
    pub post_processing_status: PostProcessingStatus,
    #[serde(default)]
    pub delivery_status: DeliveryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_intent: Option<peri_acp_types::workflow::WorkflowWriteIntent>,
    #[serde(default, skip_serializing_if = "limits_are_empty")]
    pub limits: crate::protocol::WorkflowLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<peri_acp_types::workflow::WorkflowAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_value: Option<serde_json::Value>,
    pub script: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

/// 磁盘持久化存储，管理 `.claude/workflow-runs/` 下的运行数据。
pub struct WorkflowJournalStore {
    base_dir: PathBuf,
}

impl WorkflowJournalStore {
    /// 创建 store，`cwd` 为项目工作目录。
    pub fn new(cwd: &str) -> Self {
        Self {
            base_dir: PathBuf::from(cwd).join(WORKFLOW_RUNS_DIR),
        }
    }

    /// 返回某次运行的目录路径（含防御性路径遍历检查）。
    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        // 防御性检查：run_id 不应包含路径遍历字符
        // 正常流程中 run_id 由 UUID 生成，若触发此检查说明上层校验缺失
        if run_id.contains("..") || run_id.contains('/') || run_id.contains('\\') {
            tracing::error!(
                "Refusing to construct run_dir with unsafe run_id containing path traversal chars"
            );
            // 退回 base_dir：后续文件操作将失败（找不到）而非越权访问
            return self.base_dir.clone();
        }
        self.base_dir.join(run_id)
    }

    /// 初始化运行目录，写入脚本副本。
    pub fn init_run(&self, run_id: &str, script: &str) -> std::io::Result<()> {
        let dir = self.run_dir(run_id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("script.js"), script)
    }

    /// 向 journal.jsonl 追加一条记录（每行一个 JSON 对象）。
    pub fn append(&self, run_id: &str, entry: &JournalEntry) -> std::io::Result<()> {
        let path = self.run_dir(run_id).join("journal.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = std::io::BufWriter::new(file);
        let line = serde_json::to_string(entry)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        writeln!(writer, "{line}")
    }

    /// 清空 journal.jsonl（写入空字符串截断文件）。
    pub fn truncate(&self, run_id: &str) -> std::io::Result<()> {
        let path = self.run_dir(run_id).join("journal.jsonl");
        fs::write(path, "")
    }

    /// 读取 journal.jsonl 全部条目，跳过空行和解析失败的行（宽容模式）。
    pub fn read_all(&self, run_id: &str) -> std::io::Result<Vec<JournalEntry>> {
        let path = self.run_dir(run_id).join("journal.jsonl");
        let file = File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str(trimmed) {
                entries.push(entry);
            }
        }
        entries.sort_by_key(|entry: &JournalEntry| entry.seq);
        Ok(entries)
    }

    pub fn read_attempts(
        &self,
        run_id: &str,
    ) -> std::io::Result<Vec<peri_acp_types::workflow::WorkflowAttempt>> {
        Ok(self
            .read_all(run_id)?
            .into_iter()
            .filter_map(|entry| entry.attempt)
            .collect())
    }

    /// 原子写入 state.json（先写 .tmp 再 rename，防止写到一半崩溃损坏）。
    pub fn write_state(&self, run_id: &str, state: &RunState) -> std::io::Result<()> {
        let dir = self.run_dir(run_id);
        let final_path = dir.join("state.json");
        let tmp_path = dir.join("state.json.tmp");
        let content = serde_json::to_string_pretty(state).unwrap();
        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, &final_path)
    }

    /// 清理超出 KEEP_MAX_RUNS 的最旧运行目录（按 mtime 排序）。
    pub fn cleanup_old_runs(&self) -> std::io::Result<()> {
        if !self.base_dir.exists() {
            return Ok(());
        }
        let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        let entries = fs::read_dir(&self.base_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                dirs.push((mtime, path));
            }
        }
        if dirs.len() <= KEEP_MAX_RUNS {
            return Ok(());
        }
        dirs.sort_by_key(|(t, _)| *t);
        let to_remove = dirs.len() - KEEP_MAX_RUNS;
        for (_, path) in dirs.into_iter().take(to_remove) {
            let _ = fs::remove_dir_all(path);
        }
        Ok(())
    }

    /// 列出已有 state.json 的运行 ID。
    pub fn list_runs(&self) -> Vec<String> {
        let mut runs = Vec::new();
        if !self.base_dir.exists() {
            return runs;
        }
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("state.json").exists() {
                    if let Some(name) = path.file_name() {
                        runs.push(name.to_string_lossy().into_owned());
                    }
                }
            }
        }
        runs
    }

    /// 读取并解析 state.json。
    pub fn read_state(&self, run_id: &str) -> std::io::Result<RunState> {
        let path = self.run_dir(run_id).join("state.json");
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// 将 agent 输出写入独立文件 outputs/{label}.txt。
    pub fn write_output(&self, run_id: &str, label: &str, content: &str) -> std::io::Result<()> {
        // 防御性检查：label 不含路径遍历字符
        let safe_label = if label.contains("..") || label.contains('/') || label.contains('\\') {
            "unnamed"
        } else {
            label
        };
        let dir = self.run_dir(run_id).join("outputs");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(format!("{}.txt", safe_label)), content)
    }
}

/// 递归遍历 JSON Value，将长度超过 threshold 的字符串写入 outputs/ 目录，
/// 原位置替换为 "${label}" 占位符。返回提取的文件标签列表。
pub fn extract_long_texts(
    value: &mut serde_json::Value,
    run_id: &str,
    store: &WorkflowJournalStore,
    threshold: usize,
) -> Vec<String> {
    let mut extracted = Vec::new();
    extract_long_texts_inner(value, run_id, store, threshold, "", &mut extracted);
    extracted
}

fn extract_long_texts_inner(
    value: &mut serde_json::Value,
    run_id: &str,
    store: &WorkflowJournalStore,
    threshold: usize,
    key_hint: &str,
    extracted: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let child_hint = if key_hint.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", key_hint, key)
                };
                let child = map.get_mut(&key).unwrap();
                if let serde_json::Value::String(s) = child {
                    if s.len() > threshold {
                        let label = child_hint;
                        if let Err(e) = store.write_output(run_id, &label, s) {
                            warn!(target: "workflow", run_id = %run_id, label = %label, error = %e, "write_output failed");
                        } else {
                            extracted.push(label.clone());
                        }
                        *child = serde_json::Value::String(format!("${{{}}}", label));
                    }
                } else {
                    extract_long_texts_inner(
                        child,
                        run_id,
                        store,
                        threshold,
                        &child_hint,
                        extracted,
                    );
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter_mut().enumerate() {
                let child_hint = if key_hint.is_empty() {
                    format!("[{}]", i)
                } else {
                    format!("{}[{}]", key_hint, i)
                };
                extract_long_texts_inner(item, run_id, store, threshold, &child_hint, extracted);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "journal_test.rs"]
mod tests;
