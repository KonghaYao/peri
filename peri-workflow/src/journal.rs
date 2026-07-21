//! 磁盘持久化：workflow 运行日志、状态快照、脚本副本。
//!
//! 目录结构：`.claude/workflow-runs/<runId>/`
//! - `journal.jsonl` — append-only agent() 调用结果日志（用于 cache-hit resume）
//! - `state.json` — 最终状态快照（run_done 时原子写入）
//! - `script.js` — workflow 脚本源码副本

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::protocol::JournalEntry;

const WORKFLOW_RUNS_DIR: &str = ".claude/workflow-runs";
const KEEP_MAX_RUNS: usize = 50;

/// workflow 运行的持久化状态快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_value: Option<serde_json::Value>,
    pub script: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
        let line = serde_json::to_string(entry).unwrap();
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
        Ok(entries)
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
