//! Workflow journal state and filesystem persistence.

use super::{
    fs, AcceptanceStatus, BufRead, DeliveryStatus, Deserialize, ExecutionStatus, File,
    JournalEntry, OpenOptions, PathBuf, PostProcessingStatus, Serialize, Write,
};

pub(super) const WORKFLOW_RUNS_DIR: &str = ".claude/workflow-runs";

pub(super) const KEEP_MAX_RUNS: usize = 50;

pub(super) fn limits_are_empty(limits: &crate::protocol::WorkflowLimits) -> bool {
    limits.max_agents.is_none()
        && limits.max_tool_calls.is_none()
        && limits.max_elapsed_ms.is_none()
}

pub(super) fn default_max_concurrency() -> u32 {
    3
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
    /// 交付结论的结构化原因；旧 `state.json` 缺省为 `None`，不得据此升级为通过。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_reason: Option<peri_acp_types::workflow::DeliveryReason>,
    /// Git 观测完整性；旧 `state.json` 缺省为 `None`（未记录，不等于完整观测）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcondition_observation: Option<peri_acp_types::workflow::PostconditionObservation>,
    /// 采样到的 Git 身份与可见范围；只存身份与能力，不存基线内容。
    /// 旧 `state.json` 缺省为 `None`（未记录，不得推断观测范围）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_observation_scope: Option<peri_acp_types::workflow::GitObservationScope>,
    /// 引擎级三态；与 acceptance 分开，旧 state.json 缺省为 `unknown`。
    #[serde(default)]
    pub outcome: peri_acp_types::workflow::WorkflowOutcome,
    /// run 内存在不可确认的脚本工具副作用（`outcome_unknown` / `task_accepted`，
    /// 或 run 终止时仍有 executing 的 tool 任务）。
    ///
    /// 观测事实，不是失败：`true` 才出现在 `state.json`；旧文件缺省 `false`——
    /// 不得据此推断本能力已存在。
    #[serde(
        default,
        rename = "unknownSideEffects",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub unknown_side_effects: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_intent: Option<peri_acp_types::workflow::WorkflowWriteIntent>,
    #[serde(default, skip_serializing_if = "limits_are_empty")]
    pub limits: crate::protocol::WorkflowLimits,
    /// 全部已触发的硬上限（scope / 上限 / 观测值 / attempt / 时点）。
    ///
    /// 与顶层 `error`（首条兼容文本）并存；旧 `state.json` 缺省为空，不得据此
    /// 推断“未触发过限额”。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limit_breaches: Vec<peri_acp_types::workflow::LimitBreach>,
    /// 按 attempt 累计的 token 用量；没有任何执行上报时为 `None`（不显示为 0）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_total: Option<u64>,
    /// Exact script arguments are part of the workflow input. Keeping them in
    /// state makes an ACP resume reproducible instead of silently changing the
    /// script's `args` value to `undefined`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// The concurrency setting is input state too; legacy runs used the tool
    /// default and therefore deserialize as three.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
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

/// 磁盘持久化存储，管理 `.claude/workflow-runs/` 下的运行数据。
#[derive(Debug)]
pub struct WorkflowJournalStore {
    pub(super) base_dir: PathBuf,
}

impl WorkflowJournalStore {
    /// 创建 store，`cwd` 为项目工作目录。
    #[must_use]
    pub fn new(cwd: &str) -> Self {
        Self {
            base_dir: PathBuf::from(cwd).join(WORKFLOW_RUNS_DIR),
        }
    }

    /// 返回某次运行的目录路径（含防御性路径遍历检查）。
    ///
    /// IDs containing `..` or a path separator are rejected by returning the base
    /// directory after logging an error. External IDs must be validated before journal access.
    #[must_use]
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

    /// 初始化运行目录，写入脚本副本，并创建空的 journal/tools 文件。
    ///
    /// 空文件保证「零 agent / 零工具调用」的 run 也能 resume：严格读取以文件
    /// 存在为前提，缺失只能解释为运行目录损坏。
    ///
    /// # Errors
    ///
    /// Returns the filesystem error if the run directory or script file cannot be created.
    pub fn init_run(&self, run_id: &str, script: &str) -> std::io::Result<()> {
        let dir = self.run_dir(run_id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("script.js"), script)?;
        fs::File::create(dir.join("journal.jsonl"))?;
        fs::File::create(dir.join("tools.jsonl"))?;
        Ok(())
    }

    /// 向 journal.jsonl 追加一条记录（每行一个 JSON 对象）。
    ///
    /// # Errors
    ///
    /// Returns the filesystem or serialization error if the journal entry cannot be written.
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

    /// 向 tools.jsonl 追加一条脚本工具调用记录（每行一个 JSON 对象）。
    ///
    /// append-only：`journal/truncate` 不清空本文件——回放前缀选择天然容忍断点，
    /// 清空反而会丢失已结算的工具调用事实。
    ///
    /// # Errors
    ///
    /// Returns the filesystem or serialization error if the entry cannot be written.
    pub fn append_tool(
        &self,
        run_id: &str,
        entry: &crate::protocol::ToolJournalEntry,
    ) -> std::io::Result<()> {
        let path = self.run_dir(run_id).join("tools.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = std::io::BufWriter::new(file);
        let line = serde_json::to_string(entry)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        writeln!(writer, "{line}")
    }

    /// 读取 tools.jsonl 全部条目（严格模式：坏行即失败，按 `toolSeq` 升序返回）。
    ///
    /// 文件不存在返回空列表（旧 run 没有工具调用记录）；有坏行返回
    /// `InvalidData`，与 [`Self::read_all_strict`] 同口径 fail closed。
    /// **按 `toolSeq` 排序**：并发工具调用按完成顺序追加，文件顺序不等于脚本
    /// 调用顺序；回放前缀只认 `toolSeq`，不认写入顺序。
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the file cannot be read, or `InvalidData` for malformed entries.
    pub fn read_tools_strict(
        &self,
        run_id: &str,
    ) -> std::io::Result<Vec<crate::protocol::ToolJournalEntry>> {
        let path = self.run_dir(run_id).join("tools.jsonl");
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let reader = std::io::BufReader::new(file);
        let mut entries = Vec::new();
        let paired_lines = reader.lines().enumerate();
        for (line_number, line) in paired_lines {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry = serde_json::from_str(trimmed).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "invalid tool journal entry at line {}: {error}",
                        line_number + 1
                    ),
                )
            })?;
            entries.push(entry);
        }
        entries.sort_by_key(|entry: &crate::protocol::ToolJournalEntry| entry.tool_seq);
        Ok(entries)
    }

    /// 读取 journal.jsonl 全部条目，跳过空行和解析失败的行（宽容模式）。
    pub fn read_all(&self, run_id: &str) -> std::io::Result<Vec<JournalEntry>> {
        self.read_all_impl(run_id, false)
    }

    /// Read a journal for resume without hiding corruption. The historical
    /// inspection API remains permissive, but execution recovery must fail
    /// closed when the source is missing, malformed, or has a sequence gap;
    /// an interrupted run must return to its package checkpoint instead of
    /// silently starting a fresh run.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the journal cannot be read, or `InvalidData` for malformed entries and sequence gaps.
    pub fn read_all_strict(&self, run_id: &str) -> std::io::Result<Vec<JournalEntry>> {
        self.read_all_impl(run_id, true)
    }

    pub(super) fn read_all_impl(
        &self,
        run_id: &str,
        strict: bool,
    ) -> std::io::Result<Vec<JournalEntry>> {
        let path = self.run_dir(run_id).join("journal.jsonl");
        let file = File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut entries = Vec::new();
        let paired_items = reader.lines().enumerate();
        for (line_number, line) in paired_items {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str(trimmed) {
                Ok(entry) => entries.push(entry),
                Err(error) if strict => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid journal entry at line {}: {error}", line_number + 1),
                    ));
                }
                Err(_) => {}
            }
        }
        entries.sort_by_key(|entry: &JournalEntry| entry.seq);
        if strict {
            let paired_items_2 = entries.iter().enumerate();
            for (expected_seq, entry) in paired_items_2 {
                let expected_seq = u64::try_from(expected_seq).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "journal sequence exceeds u64",
                    )
                })?;
                if entry.seq != expected_seq {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "journal sequence is not contiguous: expected seq {}, found seq {}; resume requires a workflow checkpoint replan",
                            expected_seq, entry.seq
                        ),
                    ));
                }
            }
        }
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
    ///
    /// # Errors
    ///
    /// Returns the filesystem error if the temporary snapshot cannot be written or renamed into place.
    pub fn write_state(&self, run_id: &str, state: &RunState) -> std::io::Result<()> {
        let dir = self.run_dir(run_id);
        let final_path = dir.join("state.json");
        let tmp_path = dir.join("state.json.tmp");
        let content = serde_json::to_string_pretty(state)
            .expect("RunState contains only JSON-compatible typed fields and JSON values");
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
        let path_items = dirs.into_iter().take(to_remove);
        for (_, path) in path_items {
            let _ = fs::remove_dir_all(path);
        }
        Ok(())
    }

    /// 列出已有 state.json 的运行 ID。
    #[must_use]
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

    /// 将长文本写入独立文件 `outputs/{label}.txt`。
    ///
    /// label 已由 [`output::output_label`](crate::journal::output::output_label) 归一（非法字符替换、重名去重），本方法
    /// 只做最后一道防御：仍含路径分隔符、控制字符或 `..` 形态即报错返回。
    /// **不得在这里静默改名**——静默改名会让 state.json 的 `${label}` 指向不存在
    /// 的文件（引用断裂），多个非法 label 还会互相覆盖（2026-10-11 issue）。
    pub fn write_output(&self, run_id: &str, label: &str, content: &str) -> std::io::Result<()> {
        if label.is_empty()
            || label.contains("..")
            || label.contains('/')
            || label.contains('\\')
            || label.chars().any(char::is_control)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsafe workflow output label: {label:?}"),
            ));
        }
        let dir = self.run_dir(run_id).join("outputs");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(format!("{label}.txt")), content)
    }
}
