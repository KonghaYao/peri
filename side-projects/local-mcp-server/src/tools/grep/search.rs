//! Grep 搜索引擎：遍历、匹配、排序、截断与落盘（迁移自源 `grep.rs::execute_search`）。
//!
//! 关键语义（与源逐条对应）：
//!
//! - 遍历使用 `ignore::WalkBuilder`：`hidden(true)`（跳过隐藏项）、`git_ignore(true)`、
//!   `git_exclude(true)`、`ignore(true)`、`parents(true)`；线程数
//!   `min(available_parallelism, SEARCH_THREADS_MAX)`；`max_depth` 显式设置时才限制。
//! - `glob` 过滤器预编译为 `glob::Pattern`，编译失败**静默丢弃**，匹配对象是
//!   文件 basename。
//! - 二进制检测 `BinaryDetection::quit(b'\0')`；搜索出错的文件跳过。
//! - 结果跨文件按 `display_path` 字典序稳定排序，文件内保持行序（并行遍历的
//!   push 顺序不确定，不排序则输出不稳定）。
//! - 行数截断只在 `lines.len() > head_limit` 时触发并落盘**全量**；字节预算在
//!   行数截断后兜底，保留头部行并落盘全量。
//! - `offset` 在最终字符串（含截断提示）上跳过前 N 行。
//! - 截断事实（行数/字节/单行）以**标志与计数**随 [`SearchOutcome`] 返回，另附落盘路径；
//!   调用方不再从交付文本里推断截断（GAP-023）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use grep::regex::RegexMatcherBuilder;
use grep::searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;

use super::args::{OutputMode, ParsedArgs};
use super::format::SearchSink;
use super::{MAX_OUTPUT_BYTES, SEARCH_THREADS_MAX};
use crate::tasks::log::OutputPersist;

/// 搜索失败（不可继续）；文本层由调用方加 `Error: ` 前缀（源行为）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SearchError {
    /// 已脱敏的失败说明。
    pub message: String,
}

impl SearchError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 一次搜索的结构化结果。
///
/// 截断事实全部是**计数/标志**（本模块自己产生），不是对交付文本做子串匹配：正文里出现
/// `[Output truncated:` 或 `… [line truncated]` 只说明**匹配内容本身**含这些字面量
/// （GAP-023：旧实现按子串判定，133 字节、无截断的调用被误报 `truncated=true`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutcome {
    /// 交付文本（宿主路径表示，逐字外发）。
    pub text: String,
    /// 行数截断（`head_limit` 生效）。
    pub truncated_lines: bool,
    /// 字节预算截断（超出 [`super::MAX_OUTPUT_BYTES`]）。
    pub truncated_bytes: bool,
    /// 被单行字节上限（[`super::MAX_LINE_BYTES`]）截断的行数。
    pub trimmed_lines: usize,
    /// 截断落盘路径（宿主绝对路径；只有真的落盘时存在）。
    pub persisted_path: Option<String>,
    /// 条目级纵深防御跳过的越界条目数（真实路径落在锚定根外）。
    ///
    /// 只计数，不进入交付文本、不落盘、不改变任何既有字段语义：这些条目**没有被读取**。
    /// 生产上恒为 0（搜索根已由 capability 与 [`execute_search`] 两处收敛）；非 0 表示
    /// 遍历期间发生了根内路径被换成根外目标的替换，此时结果里缺少的正是那些条目。
    pub skipped_outside: usize,
}

impl SearchOutcome {
    /// 是否发生了任何形式的截断（行数 / 字节 / 单行）。
    pub fn truncated(&self) -> bool {
        self.truncated_lines || self.truncated_bytes || self.trimmed_lines > 0
    }
}

/// 把搜索根收敛为**锚定根内**的 canonical 真实路径（校验对象 = 遍历对象）。
///
/// - 目标存在且在 `root_real` 内 → 返回真实路径（交给 [`WalkBuilder`] 的就是它）；
/// - 目标存在但真实路径在 `root_real` 外 → fail closed（含"根自身被换成指向根外的
///   符号链接"这一确定性构造）；
/// - 目标不存在 → 保持源实现的错误文案（`Search path does not exist: …`）；
/// - 目标存在却无法解析（权限等）→ fail closed，不猜、不退回词法路径。
fn resolve_search_root(search_path: &Path, root_real: &Path) -> Result<PathBuf, SearchError> {
    match std::fs::canonicalize(search_path) {
        Ok(real) => {
            if real == root_real || real.starts_with(root_real) {
                Ok(real)
            } else {
                Err(SearchError::new(format!(
                    "Search path resolves outside the workspace root: {}",
                    search_path.display()
                )))
            }
        }
        Err(_) if !search_path.exists() => Err(SearchError::new(format!(
            "Search path does not exist: {}",
            search_path.display()
        ))),
        Err(error) => Err(SearchError::new(format!(
            "Search path cannot be resolved inside the workspace root: {} ({error})",
            search_path.display()
        ))),
    }
}

/// 条目级纵深防御：返回位于 `root_real` 内的 canonical 真实路径；越界/不可解析返回 `None`。
///
/// 判定与读取使用**同一个**返回值，因此复核之后不会再按原路径打开（GAP-034 的
/// "判定一个实体、打开另一个实体"）。
fn verify_within_root(path: &Path, root_real: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    (real == root_real || real.starts_with(root_real)).then_some(real)
}

/// 在 `search_path` 下执行搜索并返回结构化结果。
///
/// `search_path` 必须是**已授权**的绝对路径（由 capability 解析，见
/// [`super::GrepContext::resolve`]）；`cwd` 只用于计算展示路径。
///
/// ## 边界：校验对象与遍历对象是同一实体（GAP-034）
///
/// 本函数不信任传入的 `search_path` 字符串，遍历前做两件事：
///
/// 1. **收敛搜索根**：把 `search_path` 解析为 canonical 真实路径，并要求它落在
///    `root_real` 之内；交给 [`WalkBuilder`] 的**只有**这个真实路径——不存在
///    "用一个字符串判定、再用另一个字符串打开"的窗口；
/// 2. **逐条目纵深防御**：每个将要读取的文件条目在进 searcher 之前，再复核一次它的
///    canonical 形态仍在 `root_real` 内；越界条目直接跳过（不读取、不进入结果、不落盘），
///    只累加 [`SearchOutcome::skipped_outside`]。
///
/// `root_real` 必须是**启动时锚定**的授权根真实路径（
/// [`crate::capability::RootDir::real_base`]）。**禁止**用"每次请求重新 canonicalize 根"
/// 的结果作基准：根路径被替换成指向根外的符号链接时，被校验对象与基准会指向同一个逃逸
/// 目标，包含判定恒真（GAP-034 的确定性构造）。
///
/// 代价：条目级复核是每个候选文件一次 `canonicalize`（`lstat` 逐组件）系统调用，只对
/// 通过 `is_file` 与 glob 过滤的条目执行；越界条目不会被读取。
pub fn execute_search(
    parsed: &ParsedArgs,
    cwd: &Path,
    root_real: &Path,
    search_path: &Path,
    head_limit: usize,
    cancelled: Arc<AtomicBool>,
    persist: &dyn OutputPersist,
) -> Result<SearchOutcome, SearchError> {
    let search_root = resolve_search_root(search_path, root_real)?;

    let mut matcher_builder = RegexMatcherBuilder::new();
    matcher_builder
        .case_insensitive(parsed.case_insensitive)
        .word(parsed.whole_word);
    if parsed.multiline {
        matcher_builder.multi_line(true).dot_matches_new_line(true);
    }
    if parsed.fixed_strings {
        matcher_builder.fixed_strings(true);
    }
    let matcher = matcher_builder
        .build(&parsed.pattern)
        .map_err(|error| SearchError::new(error.to_string()))?;

    let mut builder = WalkBuilder::new(&search_root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .threads(
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(SEARCH_THREADS_MAX),
        );
    if let Some(depth) = parsed.max_depth {
        builder.max_depth(Some(depth));
    }

    // glob 过滤器编译失败静默丢弃（源行为）。
    let glob_filters: Vec<glob::Pattern> = parsed
        .glob_filters
        .iter()
        .filter_map(|pattern| glob::Pattern::new(pattern).ok())
        .collect();

    let results: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let total_lines = Arc::new(AtomicUsize::new(0));
    let total_bytes = Arc::new(AtomicUsize::new(0));
    let trimmed_lines = Arc::new(AtomicUsize::new(0));
    let skipped_outside = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let matcher = Arc::new(matcher);
    let cwd_display = Arc::new(cwd.to_string_lossy().to_string());
    // 条目级复核需要把锚定根搬进并行遍历器（`WalkParallel` 的回调必须 `'static`）。
    let root_real_owned = Arc::new(root_real.to_path_buf());
    let before_context = parsed.before_context;
    let after_context = parsed.after_context;

    builder.build_parallel().run(|| {
        let matcher = Arc::clone(&matcher);
        let total_lines = Arc::clone(&total_lines);
        let total_bytes = Arc::clone(&total_bytes);
        let trimmed_lines = Arc::clone(&trimmed_lines);
        let skipped_outside = Arc::clone(&skipped_outside);
        let stopped = Arc::clone(&stopped);
        let cancelled = Arc::clone(&cancelled);
        let cwd = Arc::clone(&cwd_display);
        let root_real = Arc::clone(&root_real_owned);
        let glob_filters = glob_filters.clone();
        let results = Arc::clone(&results);

        Box::new(
            move |entry_result: Result<ignore::DirEntry, ignore::Error>| {
                use ignore::WalkState;

                let entry = match entry_result {
                    Ok(entry) => entry,
                    Err(_) => return WalkState::Continue,
                };

                // 检查点：预算置位或取消后协作退出整个遍历。
                if stopped.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    return WalkState::Continue;
                }

                if !glob_filters.is_empty() {
                    let file_name = entry.file_name().to_string_lossy();
                    if !glob_filters
                        .iter()
                        .any(|pattern| pattern.matches(&file_name))
                    {
                        return WalkState::Continue;
                    }
                }

                // 条目级纵深防御：只对**将要读取**的候选文件复核一次真实形态。
                // 越界条目直接跳过（不读取、不进结果、不落盘）。
                let entry_real = match verify_within_root(entry.path(), &root_real) {
                    Some(real) => real,
                    None => {
                        skipped_outside.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(
                            entry = %entry.path().display(),
                            "搜索条目解析到锚定根外，已跳过"
                        );
                        return WalkState::Continue;
                    }
                };

                // 展示路径：优先相对 cwd；条目已是 canonical 形态，cwd 若是词法形态
                // （根经符号链接祖先打开）再退回相对锚定根——两者都表达"相对工作区根"。
                let display_path = entry
                    .path()
                    .strip_prefix(cwd.as_str())
                    .or_else(|_| entry.path().strip_prefix(root_real.as_path()))
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .to_string();

                let mut searcher_builder = SearcherBuilder::new();
                searcher_builder
                    .line_number(parsed.line_number)
                    .binary_detection(BinaryDetection::quit(b'\x00'));
                if before_context > 0 {
                    searcher_builder.before_context(before_context);
                }
                if after_context > 0 {
                    searcher_builder.after_context(after_context);
                }
                if parsed.multiline {
                    searcher_builder.multi_line(true);
                }
                searcher_builder.invert_match(parsed.invert_match);
                let mut searcher = searcher_builder.build();

                let mut sink = SearchSink {
                    output_mode: parsed.output_mode,
                    local_lines: Vec::new(),
                    total_lines: Arc::clone(&total_lines),
                    total_bytes: Arc::clone(&total_bytes),
                    trimmed_lines: Arc::clone(&trimmed_lines),
                    max_limit: head_limit,
                    cancelled: Arc::clone(&cancelled),
                    stopped: Arc::clone(&stopped),
                    display_path: display_path.clone(),
                    match_count: std::cell::Cell::new(0),
                    has_match: std::cell::Cell::new(false),
                    after_context,
                    before_context,
                    show_line_numbers: parsed.line_number,
                };

                if searcher
                    .search_path(&*matcher, &entry_real, &mut sink)
                    .is_err()
                {
                    // 二进制文件等搜索错误：跳过该文件（源行为）。
                    return WalkState::Continue;
                }

                // Default 模式：本地缓冲批量入共享（每文件一次锁）。
                if !sink.local_lines.is_empty() {
                    let mut guard = results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.extend(
                        sink.local_lines
                            .drain(..)
                            .map(|line| (display_path.clone(), line)),
                    );
                }

                // 非 Default 模式每文件恰 1 行输出（head_limit 语义 = 前 N 个文件）。
                let file_line = if parsed.output_mode == OutputMode::FilesOnly
                    && sink.has_match.get()
                {
                    Some(display_path.clone())
                } else if parsed.output_mode == OutputMode::CountOnly && sink.match_count.get() > 0
                {
                    Some(format!("{}:{}", display_path, sink.match_count.get()))
                } else if parsed.output_mode == OutputMode::FilesWithoutMatch
                    && !sink.has_match.get()
                {
                    Some(display_path.clone())
                } else {
                    None
                };
                if let Some(line) = file_line {
                    let bytes = line.len();
                    let mut guard = results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.push((display_path.clone(), line));
                    total_lines.fetch_add(1, Ordering::Relaxed);
                    if total_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes > MAX_OUTPUT_BYTES {
                        stopped.store(true, Ordering::Relaxed);
                    }
                }

                // 文件级预算：达到 head_limit 后不再进入新文件（所有模式统一）。
                if head_limit > 0 && total_lines.load(Ordering::Relaxed) >= head_limit {
                    stopped.store(true, Ordering::Relaxed);
                }

                if stopped.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            },
        )
    });

    let mut guard = results
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 跨文件按 display_path 字典序稳定排序；文件内行序保持。
    guard.sort_by(|a, b| a.0.cmp(&b.0));
    if guard.is_empty() {
        return Ok(SearchOutcome {
            text: "No matches found.".to_string(),
            truncated_lines: false,
            truncated_bytes: false,
            trimmed_lines: trimmed_lines.load(Ordering::Relaxed),
            persisted_path: None,
            skipped_outside: skipped_outside.load(Ordering::Relaxed),
        });
    }
    let lines: Vec<String> = guard.iter().map(|(_, line)| line.clone()).collect();
    drop(guard);

    let joined = lines.join("\n");
    let mut output = joined.clone();
    let mut line_note = String::new();
    let mut persisted_path: Option<String> = None;
    let mut truncated_lines = false;
    let mut truncated_bytes = false;

    // 行数精确截断：恰好 head_limit 行不标 truncated。
    if head_limit > 0 && lines.len() > head_limit {
        let persisted = persist.persist(&joined);
        truncated_lines = true;
        persisted_path = persisted
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string());
        output = lines[..head_limit].join("\n");
        line_note = format!("\n... (truncated at {head_limit} lines)");
        line_note.push_str(&persisted.hint);
    }

    // 字节预算兜底：行数截断后仍可能超限（head_limit=0 或超长行累积）。
    if output.len() > MAX_OUTPUT_BYTES {
        let persisted = persist.persist(&joined);
        truncated_bytes = true;
        // 交付文本里出现的是**这一次**的提示，结构化字段必须指向同一个产物。
        persisted_path = persisted
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string());
        let mut head: Vec<&str> = Vec::new();
        let mut head_bytes = 0usize;
        for line in output.split('\n') {
            if !head.is_empty() && head_bytes + line.len() + 1 > MAX_OUTPUT_BYTES {
                break;
            }
            head.push(line);
            head_bytes += line.len() + 1;
        }
        output = super::byte_overflow_text(
            &head.join("\n"),
            lines.len(),
            joined.len(),
            head.len(),
            &persisted.hint,
        );
    } else if !line_note.is_empty() {
        output.push_str(&line_note);
    }

    Ok(SearchOutcome {
        text: output,
        truncated_lines,
        truncated_bytes,
        trimmed_lines: trimmed_lines.load(Ordering::Relaxed),
        persisted_path,
        skipped_outside: skipped_outside.load(Ordering::Relaxed),
    })
}

/// 对最终输出应用 `offset`：跳过前 N 行（源在截断之后应用）。
pub fn apply_offset(output: String, offset: Option<usize>) -> String {
    match offset {
        Some(offset) if offset > 0 => {
            let lines: Vec<&str> = output.split('\n').collect();
            lines
                .into_iter()
                .skip(offset)
                .collect::<Vec<&str>>()
                .join("\n")
        }
        _ => output,
    }
}
