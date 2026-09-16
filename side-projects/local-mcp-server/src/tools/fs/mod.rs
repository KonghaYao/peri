//! Read/Write/Edit/Glob/folder_operations 的语义、事务与调用入口（WP-002 独占）。
//!
//! ## 角色
//!
//! - [`FsRuntime`] 在本进程内执行真实文件系统操作（[`FsContext`] 提供 capability 根、
//!   参数与路径）。工具语义、错误文案、限额与源实现逐字对齐。
//! - [`FsCall`] 是本模块的调用形状：调用方（[`crate::runtime`]）把已翻译的路径与
//!   原始参数交进来，拿到冻结形状的 [`crate::wire::ToolResponse`]（业务错误也在
//!   `is_error=true` 的结果里）。
//!
//! 路径授权只在 capability 层判定；调用方传入的路径仍会被 [`FsRuntime`] 复核一次
//! （纵深防御，不因为"上游已翻译"而放宽）。
//!
//! ## 常量
//!
//! [`limits`] 里的数值全部来自源实现，**不得**改成运行时可配置项（WP-001 冻结规则）。

pub mod args;
pub mod draft;
pub mod edit;
pub mod folder;
pub mod glob;
pub mod read;
pub mod sentinel;
pub mod transaction;
pub mod write;

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::capability::{AccessError, RequestedPath, RootDir};
use crate::error::ToolError;
use crate::output::Persister;
use crate::wire::{StructuredOutput, ToolResponse};

use self::draft::DraftStore;
use self::transaction::TargetLocks;

/// 五个文件系统工具（顺序与 [`crate::wire::TOOL_NAMES`] 的前五项一致）。
pub const FS_TOOL_NAMES: [&str; 5] = ["Read", "Write", "Edit", "Glob", "folder_operations"];

/// 是否由本包负责的工具。
pub fn is_fs_tool(name: &str) -> bool {
    FS_TOOL_NAMES.contains(&name)
}

/// 工具的路径参数名（Glob 的 `path` 可选，缺省为工作区根）。
pub fn path_argument(tool: &str) -> Option<&'static str> {
    match tool {
        "Read" | "Write" | "Edit" => Some("file_path"),
        "Glob" => Some("path"),
        "folder_operations" => Some("folder_path"),
        _ => None,
    }
}

/// 工具语义常量（与源实现逐字一致）。
pub mod limits {
    use std::time::Duration;

    /// Read：默认读取行数上限。
    pub const READ_MAX_LINES: usize = 2000;
    /// Read：文件大小上限（32 MiB，offset/limit 不能绕过）。
    pub const READ_MAX_FILE_SIZE: u64 = 32 * 1024 * 1024;
    /// Read：单行字符上限（超出前置截断标记）。
    pub const READ_MAX_CHARS_PER_LINE: usize = 65536;
    /// Read：输出字节上限（按整行预算截断，**不落盘**）。
    pub const READ_MAX_OUTPUT_BYTES: usize = 5_000;
    /// Glob：结果条数上限（达到即早停）。
    pub const GLOB_MAX_RESULTS: usize = 1_000;
    /// Glob：输出字节上限。
    pub const GLOB_MAX_OUTPUT_BYTES: usize = 20_000;
    /// Glob：字节超限时内联保留的结果条数。
    pub const GLOB_HEAD_RESULTS_ON_BYTES_OVERFLOW: usize = 100;
    /// Glob：扫描超时。
    pub const GLOB_SCAN_TIMEOUT: Duration = Duration::from_secs(15);
    /// `folder_operations`：listing/deep_scan 条目上限。
    pub const FOLDER_MAX_LIST_ENTRIES: usize = 500;
    /// 遍历与 listing 跳过的目录名（17 项，源 `should_skip_dir`）。
    pub const SKIP_DIRS: [&str; 17] = [
        "node_modules",
        ".git",
        "dist",
        "build",
        ".next",
        ".turbo",
        "coverage",
        ".nyc_output",
        "temp",
        ".cache",
        "vendor",
        "venv",
        "__pycache__",
        "target",
        "out",
        ".output",
        "worktrees",
    ];
}

/// 遍历黑名单判定（源 `should_skip_dir`）。
pub fn should_skip_dir(name: &str) -> bool {
    limits::SKIP_DIRS.contains(&name)
}

/// Glob 字节超限时的交付文案。
///
/// 工具自身的字节兜底与交付侧预算复核都要产出同一条提示，因此格式只在这里
/// 定义一次；`bytes` 是**判定所用表示**的字节数。
pub(crate) fn glob_byte_overflow_text(
    head: &str,
    count: usize,
    bytes: usize,
    head_count: usize,
    hint: &str,
) -> String {
    format!(
        "{head}\n\n[Output truncated: {count} files total, {bytes} bytes; showing first {head_count} — exceeds {} byte limit]{hint}",
        limits::GLOB_MAX_OUTPUT_BYTES
    )
}

// ───────────────────────────────── 载荷与结果 ────────────────────────────────

/// FS 调用载荷（WP-002 冻结）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsCall {
    /// 规范工具名（已解析别名）。
    pub tool: String,
    /// 原始参数（逐字保留：用于类型校验与「回显原始路径」的错误文案）。
    pub arguments: serde_json::Value,
    /// 解析后的**宿主**绝对路径；路径参数存在且是合法字符串时必填。
    ///
    /// `None` 只出现在「路径参数缺失或不是字符串」的调用里——那种调用一定会
    /// 先失败于必需参数校验，因此不需要路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// FS 工具成功结果。
#[derive(Debug, Clone)]
pub struct FsOutcome {
    /// 人类可读文本（宿主路径形态，逐字外发）。
    pub text: String,
    /// 输出是否被截断。
    pub truncated: bool,
    /// 落盘路径（宿主绝对路径）。
    pub persisted_path: Option<String>,
    /// 工具自有结构化字段。
    pub extra: Vec<(&'static str, serde_json::Value)>,
}

impl FsOutcome {
    /// 纯文本结果。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            truncated: false,
            persisted_path: None,
            extra: Vec::new(),
        }
    }

    /// 追加结构化字段。
    pub fn with_extra(mut self, key: &'static str, value: serde_json::Value) -> Self {
        self.extra.push((key, value));
        self
    }

    /// 记录落盘路径（同时置 `truncated=true`）。
    pub fn with_persisted(mut self, path: Option<String>) -> Self {
        if path.is_some() {
            self.truncated = true;
        }
        self.persisted_path = path;
        self
    }

    /// 标记截断。
    pub fn truncated(mut self) -> Self {
        self.truncated = true;
        self
    }
}

/// FS 工具业务失败（投影为 `isError=true` 的 tool result）。
#[derive(Debug, Clone)]
pub struct FsFailure {
    /// 面向调用方的文本（保留源实现文案）。
    pub message: String,
    /// 结构化补充字段（如草稿 id）。
    pub extra: Vec<(&'static str, serde_json::Value)>,
}

impl FsFailure {
    /// 纯文本失败。
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            extra: Vec::new(),
        }
    }

    /// 追加结构化字段。
    pub fn with_extra(mut self, key: &'static str, value: serde_json::Value) -> Self {
        self.extra.push((key, value));
        self
    }
}

// ──────────────────────────── 进程内执行 ────────────────────────────

/// FS 执行器（本进程内唯一持有授权根句柄的组件）。
#[derive(Debug)]
pub struct FsRuntime {
    root: RootDir,
    drafts: Mutex<Option<DraftStore>>,
    locks: &'static TargetLocks,
    persister: Persister,
}

impl FsRuntime {
    /// 以宿主工作区根构造；草稿开关按环境变量读取一次。
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, AccessError> {
        Self::with_artifact_dir(root, crate::output::DEFAULT_ARTIFACT_DIR)
    }

    /// 指定落盘产物目录（相对授权根）。
    pub fn with_artifact_dir(
        root: impl Into<PathBuf>,
        artifact_dir: impl Into<String>,
    ) -> Result<Self, AccessError> {
        let root = RootDir::open(root)?;
        Ok(Self {
            root,
            drafts: Mutex::new(draft::draft_enabled().then(DraftStore::new)),
            locks: transaction::global_locks(),
            persister: Persister::new(artifact_dir),
        })
    }

    /// 授权根句柄。
    pub fn root(&self) -> &RootDir {
        &self.root
    }

    /// 截断输出落盘器。
    pub fn persister(&self) -> &Persister {
        &self.persister
    }

    /// 进程级 per-target 锁表。
    pub fn locks(&self) -> &'static TargetLocks {
        self.locks
    }

    /// 保存草稿（草稿关闭时返回 `None`）。
    pub fn save_draft(&self, target: &str, content: &str, append: bool) -> Option<String> {
        self.drafts
            .lock()
            .as_mut()
            .map(|store| store.save(target, content.to_string(), append))
    }

    /// 在草稿锁内执行操作（草稿关闭时返回 `None`）。
    pub fn with_draft_store<T>(&self, operation: impl FnOnce(&mut DraftStore) -> T) -> Option<T> {
        self.drafts.lock().as_mut().map(operation)
    }

    /// 执行一次 FS 调用。
    ///
    /// 协议级失败（工具名不在五工具内、参数不是对象、路径不在根内）返回
    /// `Err(ToolError)`；工具业务失败是 `Ok(ToolResponse::tool_error)`。
    pub fn call(&self, call: &FsCall) -> Result<ToolResponse, ToolError> {
        if !is_fs_tool(&call.tool) {
            return Err(ToolError::Internal {
                message: format!("`{}` is not a filesystem tool", call.tool),
            });
        }
        if !call.arguments.is_object() {
            return Err(ToolError::InvalidRequest {
                message: format!("Tool `{}` arguments must be a JSON object.", call.tool),
            });
        }

        let requested = self.resolve_call_path(call)?;
        let context = FsContext {
            runtime: self,
            arguments: &call.arguments,
            requested: &requested,
        };
        let outcome = match call.tool.as_str() {
            "Read" => read::execute(&context),
            "Write" => write::execute(&context),
            "Edit" => edit::execute(&context),
            "Glob" => glob::execute(&context),
            "folder_operations" => folder::execute(&context),
            other => Err(FsFailure::text(format!("Unknown tool: {other}"))),
        };
        Ok(project(&call.tool, outcome))
    }

    /// 把 `FsCall.path` 校验为授权根内的相对路径（路由层已解析，这里仍复核一次）。
    fn resolve_call_path(&self, call: &FsCall) -> Result<RequestedPath, ToolError> {
        let Some(path) = call.path.as_deref() else {
            // 路径参数缺失/非字符串：工具会先给出必需参数错误，这里给一个安全的默认根。
            return Ok(RequestedPath::from_components(&[], self.root.base()));
        };
        self.root
            .request_path(path)
            .map_err(|error| ToolError::InvalidRequest {
                message: error.message(),
            })
    }
}

/// 一次工具调用上下文（进程内执行）。
#[derive(Debug)]
pub struct FsContext<'a> {
    runtime: &'a FsRuntime,
    arguments: &'a serde_json::Value,
    requested: &'a RequestedPath,
}

impl<'a> FsContext<'a> {
    /// 执行器。
    pub fn runtime(&self) -> &'a FsRuntime {
        self.runtime
    }

    /// 授权根。
    pub fn root(&self) -> &'a RootDir {
        self.runtime.root()
    }

    /// 归一后的相对路径。
    pub fn requested(&self) -> &'a RequestedPath {
        self.requested
    }

    /// 相对授权根的显示路径（与源实现 `strip_prefix(cwd)` 等价）。
    pub fn relative_path(&self) -> String {
        self.requested.relative()
    }

    /// 宿主绝对显示路径（未做符号链接解析，见模块文档）。
    pub fn display_path(&self) -> PathBuf {
        let mut path = self.root().base().to_path_buf();
        for component in self.requested().components() {
            path.push(component);
        }
        path
    }

    /// 把 capability 失败投影为工具失败。
    ///
    /// 需要区分「不存在」的工具（Read/Edit）会在此之前自行处理；这里是通用出口，
    /// 文案与源实现把底层错误原样外发时的形态一致。
    pub fn access_failure(&self, error: AccessError) -> FsFailure {
        FsFailure::text(error.io_text())
    }

    /// per-target 锁键（相对授权根；同目标串行）。
    pub fn target_key(&self) -> String {
        self.requested().relative()
    }

    /// per-target 锁键的路径形态。
    pub fn target_key_path(&self) -> PathBuf {
        Path::new(&self.target_key()).to_path_buf()
    }

    /// 遍历的起点组件（授权根内的相对组件）。
    pub fn walk_start(&self) -> Vec<String> {
        self.requested().components().to_vec()
    }
}

fn project(tool: &str, outcome: Result<FsOutcome, FsFailure>) -> ToolResponse {
    match outcome {
        Ok(outcome) => {
            let mut structured = StructuredOutput::ok(tool);
            structured.truncated = outcome.truncated;
            structured.persisted_path = outcome.persisted_path;
            for (key, value) in outcome.extra {
                structured = structured.with_extra(key, value);
            }
            ToolResponse::ok(outcome.text, structured)
        }
        Err(failure) => {
            let mut structured = StructuredOutput::error(tool);
            for (key, value) in failure.extra {
                structured = structured.with_extra(key, value);
            }
            ToolResponse::tool_error(failure.message, structured)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_tool_surface_matches_wire_order() {
        // 五个 FS 工具在冻结注册面里的相对顺序必须与本模块常量一致
        // （`TOOL_NAMES` 的第 5 项是 Grep，不属于本包）。
        let from_wire: Vec<&str> = crate::wire::TOOL_NAMES
            .iter()
            .copied()
            .filter(|name| is_fs_tool(name))
            .collect();
        assert_eq!(from_wire, FS_TOOL_NAMES.to_vec());
        assert!(is_fs_tool("Read"));
        assert!(is_fs_tool("folder_operations"));
        assert!(!is_fs_tool("Grep"));
        assert!(!is_fs_tool("Bash"));
    }

    #[test]
    fn test_skip_dirs_matches_source_list() {
        assert_eq!(limits::SKIP_DIRS.len(), 17);
        assert!(should_skip_dir("node_modules"));
        assert!(should_skip_dir(".git"));
        assert!(!should_skip_dir("src"));
    }

    #[test]
    fn test_path_argument_covers_five_tools() {
        assert_eq!(path_argument("Read"), Some("file_path"));
        assert_eq!(path_argument("Write"), Some("file_path"));
        assert_eq!(path_argument("Edit"), Some("file_path"));
        assert_eq!(path_argument("Glob"), Some("path"));
        assert_eq!(path_argument("folder_operations"), Some("folder_path"));
        assert_eq!(path_argument("Grep"), None);
    }
}
