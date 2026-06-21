use std::{
    cell::Cell,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use grep::{
    regex::RegexMatcherBuilder,
    searcher::{BinaryDetection, SearcherBuilder},
};
use ignore::WalkBuilder;
use peri_agent::tools::BaseTool;
use serde_json::Value;
use tokio::time::{timeout, Duration};

/// Grep tool - 与 Claude Code Grep 工具对齐
pub struct GrepTool {
    pub cwd: String,
}

impl GrepTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

const GREP_DESCRIPTION: &str = r#"A powerful search tool built on ripgrep. Supports full regex syntax (e.g. "log.*Error", "function\s+\w+"). Filter files with glob parameter (e.g. "*.js", "*.{ts,tsx}") or type parameter (e.g. "js", "py", "rust", "go"). Use output_mode to control result format.

Usage:
- Always provide pattern parameter
- Use glob parameter for file type filtering (e.g. "*.js", "*.{ts,tsx}")
- Use type parameter for language-based filtering (e.g. "rust", "js", "py")
- Supports full regex syntax — literal braces need escaping (use \{\} to find interface{} in Go code)
- Output includes line numbers by default (use -n to disable)
- Search times out after 15 seconds; use more specific patterns for large codebases
- Default head_limit is 250 lines; use sparingly for large result sets
- Use fixed_strings (-F) to search literal strings without regex interpretation
- Use invert_match (-v) to find lines that do NOT match the pattern
- Use whole_word (-w) to match whole words only
- Use multiline to match patterns spanning multiple lines
- Use max_depth to limit search directory depth

Output modes:
- "content": shows matching lines with line numbers (default)
- "files_with_matches": lists only file paths that contain matches
- "count": shows match counts per file
- "files_without_matches": lists only file paths that do NOT contain matches

Context control:
- -C: symmetric context lines before and after each match
- -A: context lines after each match (takes priority over -C)
- -B: context lines before each match (takes priority over -C)

When to use:
- Prefer Grep over Bash commands like grep or rg for content search
- Use Glob for file name search, Grep for content search
- For open-ended searches, start with the most specific query and broaden if needed"#;

use super::{
    grep_args::{GrepInput, OutputMode, ParsedArgs},
    grep_format::SearchSink,
};
use crate::tools::output_persist::persist_truncated_output;

/// 核心搜索函数（同步，在 spawn_blocking 中运行）
fn execute_search(
    parsed: &ParsedArgs,
    cwd: &str,
    head_limit: usize,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // 构建搜索路径
    let search_path = match &parsed.path {
        Some(p) => {
            let p = Path::new(p);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                Path::new(cwd).join(p)
            }
        }
        None => PathBuf::from(cwd),
    };

    if !search_path.exists() {
        return Err(format!("Search path does not exist: {}", search_path.display()).into());
    }

    // 构建 RegexMatcher
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
    let matcher = matcher_builder.build(&parsed.pattern)?;

    // 构建 WalkBuilder
    let mut builder = WalkBuilder::new(&search_path);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .threads(num_cpus::get());
    if let Some(depth) = parsed.max_depth {
        builder.max_depth(Some(depth));
    }

    // 预编译 glob 过滤器
    let glob_filters: Vec<glob::Pattern> = parsed
        .glob_filters
        .iter()
        .filter_map(|g| glob::Pattern::new(g).ok())
        .collect();

    // 共享状态
    let results = Arc::new(Mutex::new(Vec::new()));
    let total_lines = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let matcher = Arc::new(matcher);
    let cwd = Arc::new(cwd.to_string());
    let before_context = parsed.before_context;
    let after_context = parsed.after_context;

    // 并行搜索
    builder.build_parallel().run(|| {
        let matcher = Arc::clone(&matcher);
        let total_lines = Arc::clone(&total_lines);
        let stopped = Arc::clone(&stopped);
        let cwd = Arc::clone(&cwd);
        let glob_filters = glob_filters.clone();
        let results = Arc::clone(&results);

        Box::new(
            move |entry_result: Result<ignore::DirEntry, ignore::Error>| {
                use ignore::WalkState;

                let entry = match entry_result {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };

                if stopped.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    return WalkState::Continue;
                }

                // -g glob 过滤
                if !glob_filters.is_empty() {
                    let file_name = entry.file_name().to_string_lossy();
                    if !glob_filters.iter().any(|p| p.matches(&file_name)) {
                        return WalkState::Continue;
                    }
                }

                // 显示路径：相对于 cwd 的路径
                let display_path = entry
                    .path()
                    .strip_prefix(cwd.as_str())
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
                    results: Arc::clone(&results),
                    total_lines: Arc::clone(&total_lines),
                    max_limit: head_limit,
                    stopped: Arc::clone(&stopped),
                    display_path: display_path.clone(),
                    match_count: Cell::new(0),
                    has_match: Cell::new(false),
                    after_context,
                    before_context,
                    show_line_numbers: parsed.line_number,
                };

                match searcher.search_path(&*matcher, entry.path(), &mut sink) {
                    Ok(_) => {}
                    Err(_) => {
                        // 二进制文件等错误，跳过
                        return WalkState::Continue;
                    }
                }

                // FilesOnly / CountOnly / FilesWithoutMatch 模式在搜索完成后处理
                if parsed.output_mode == OutputMode::FilesOnly && sink.has_match.get() {
                    let mut r = results.lock().unwrap();
                    r.push(display_path.clone());
                } else if parsed.output_mode == OutputMode::CountOnly && sink.match_count.get() > 0
                {
                    let mut r = results.lock().unwrap();
                    r.push(format!("{}:{}", display_path, sink.match_count.get()));
                } else if parsed.output_mode == OutputMode::FilesWithoutMatch
                    && !sink.has_match.get()
                {
                    let mut r = results.lock().unwrap();
                    r.push(display_path.clone());
                }

                if stopped.load(Ordering::Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            },
        )
    });

    // 格式化输出
    let results = results.lock().unwrap();
    if results.is_empty() {
        return Ok("No matches found.".to_string());
    }

    let mut output = results.join("\n");
    let total = total_lines.load(Ordering::Relaxed);
    if total >= head_limit && head_limit > 0 {
        let persist_hint = persist_truncated_output(&output);
        output.push_str(&format!("\n... (truncated at {} lines)", head_limit));
        output.push_str(&persist_hint);
    }

    Ok(output)
}

#[async_trait::async_trait]
impl BaseTool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        GREP_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regular expression pattern to search for in file contents. Supports full regex syntax (e.g. \"log.*Error\", \"function\\s+\\w+\")"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory path to search in. Defaults to current working directory if not specified"
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. \"*.js\", \"*.{ts,tsx}\"). Only files matching the glob will be searched"
                },
                "type": {
                    "type": "string",
                    "description": "Filter files by type. Common values: \"rust\", \"js\", \"py\", \"go\", \"java\", \"ts\". More efficient than glob for type-based filtering"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count", "files_without_matches"],
                    "description": "Output mode: \"content\" shows matching lines with line numbers (default), \"files_with_matches\" lists only file paths, \"count\" shows match counts per file, \"files_without_matches\" lists file paths without matches"
                },
                "-i": {
                    "type": "boolean",
                    "description": "Enable case-insensitive search (default: false). Alias of case_insensitive"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Enable case-insensitive search (default: false). Semantic alias of -i"
                },
                "-C": {
                    "type": "number",
                    "description": "Number of context lines to show before and after each match. Alias of context"
                },
                "context": {
                    "type": "number",
                    "description": "Number of context lines to show before and after each match. Semantic alias of -C"
                },
                "-A": {
                    "type": "number",
                    "description": "Number of context lines to show after each match (takes priority over -C). Alias of after_context"
                },
                "after_context": {
                    "type": "number",
                    "description": "Number of context lines to show after each match (takes priority over context). Semantic alias of -A"
                },
                "-B": {
                    "type": "number",
                    "description": "Number of context lines to show before each match (takes priority over -C). Alias of before_context"
                },
                "before_context": {
                    "type": "number",
                    "description": "Number of context lines to show before each match (takes priority over context). Semantic alias of -B"
                },
                "-n": {
                    "type": "boolean",
                    "description": "Show line numbers (default: true). Alias of show_line_numbers"
                },
                "show_line_numbers": {
                    "type": "boolean",
                    "description": "Show line numbers (default: true). Semantic alias of -n"
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable multiline mode where ^/$ match line boundaries and . matches newlines (default: false)"
                },
                "whole_word": {
                    "type": "boolean",
                    "description": "Match whole words only (default: false)"
                },
                "invert_match": {
                    "type": "boolean",
                    "description": "Invert match: show lines that do NOT match the pattern, equivalent to grep -v (default: false)"
                },
                "fixed_strings": {
                    "type": "boolean",
                    "description": "Treat pattern as a literal string instead of regex, equivalent to grep -F (default: false)"
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum directory depth to search. Limits how deep the search traverses into subdirectories"
                },
                "head_limit": {
                    "type": "number",
                    "description": "Limit output to first N matching lines (default 250). Pass 0 for unlimited. Use sparingly — large result sets waste context"
                },
                "offset": {
                    "type": "number",
                    "description": "Skip first N lines of output before applying head_limit"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let pattern = match input.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return Err("Error: Missing required parameter 'pattern'".into()),
        };

        let grep_input = GrepInput {
            pattern,
            path: input
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            glob: input
                .get("glob")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            type_filter: input
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            output_mode: input
                .get("output_mode")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            case_insensitive: input
                .get("case_insensitive")
                .or_else(|| input.get("-i"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            context: input
                .get("context")
                .or_else(|| input.get("-C"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize),
            before_context: input
                .get("before_context")
                .or_else(|| input.get("-B"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize),
            after_context: input
                .get("after_context")
                .or_else(|| input.get("-A"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize),
            line_number: input
                .get("show_line_numbers")
                .or_else(|| input.get("-n"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            multiline: input
                .get("multiline")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            whole_word: input
                .get("whole_word")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            invert_match: input
                .get("invert_match")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            fixed_strings: input
                .get("fixed_strings")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            head_limit: input
                .get("head_limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(250) as usize,
            offset: input
                .get("offset")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize),
            max_depth: input
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize),
        };

        let parsed = match grep_input.to_parsed_args() {
            Ok(p) => p,
            Err(e) => return Err(format!("Error: {e}").into()),
        };

        let head_limit = grep_input.head_limit;

        let cwd = self.cwd.clone();
        let result = timeout(
            Duration::from_secs(15),
            tokio::task::spawn_blocking(move || execute_search(&parsed, &cwd, head_limit)),
        )
        .await;

        // offset 后处理（在超时/结果后应用）
        let output =
            match result {
                Err(_) => return Err(
                    "Error: Search timed out after 15 seconds. Please use a more specific pattern."
                        .into(),
                ),
                Ok(Err(e)) => return Err(format!("Error: {e}").into()),
                Ok(Ok(Ok(output))) => output,
                Ok(Ok(Err(e))) => return Err(format!("Error: {e}").into()),
            };

        // 应用 offset：跳过前 N 行
        let final_output = if let Some(offset) = grep_input.offset {
            if offset > 0 {
                let lines: Vec<&str> = output.split('\n').collect();
                let skipped: Vec<&str> = lines.into_iter().skip(offset).collect();
                skipped.join("\n")
            } else {
                output
            }
        } else {
            output
        };

        Ok(final_output)
    }
}

#[cfg(test)]
#[path = "grep_test.rs"]
mod tests;
