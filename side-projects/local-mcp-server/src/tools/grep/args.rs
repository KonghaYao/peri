//! Grep 参数解析：公开字段、默认值、别名优先级与语义校验。
//!
//! 本文件是源实现（`peri-middlewares/src/tools/filesystem/grep.rs`、
//! `grep_args.rs`、`GrepTool::parameters()`）的逐字段迁移，规则如下：
//!
//! - `head_limit` 默认 `250`，`0` = unlimited；`offset`/`max_depth` 允许 `0`，
//!   未传则为 `None`。
//! - 语义别名（`case_insensitive`/`context`/`before_context`/`after_context`/
//!   `show_line_numbers`）**优先于** CLI 风格别名（`-i`/`-C`/`-B`/`-A`/`-n`）：
//!   源实现用 `input.get(语义键).or_else(|| input.get(CLI 键))`，因此语义键一旦
//!   **存在**就独占解析；其值类型不合法时回落到默认值而**不会**再读 CLI 键。
//! - 数值参数经 `as_f64` 判整：小数、负数、非数值都拒绝，错误文案与源逐字一致。
//! - `type` 通过 [`type_to_glob`] 映射为 glob 过滤器；未知 `type` 映射为空列表，
//!   等于**不过滤**（源实现事实，保留而非"修正"）。
//! - `glob` 过滤器编译失败会被静默丢弃（源实现事实），匹配对象是文件 **basename**。
//! - 未知字段一律忽略（源实现只读取已知键；`test_bash_legacy_params_ignored`
//!   同族行为在 Grep 侧为静默忽略）。

use serde_json::Value;

/// `head_limit` 默认值（源：`grep.rs` invoke 的 `None => 250`）。
pub const DEFAULT_HEAD_LIMIT: usize = 250;

/// 参数级失败；`message` 是可直接进入 tool error 文本的源文案。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GrepArgError {
    /// 源文案（可能含调用方提供的参数值，不含宿主路径或 secret）。
    pub message: String,
}

impl GrepArgError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 输出模式（源：`grep_args.rs` `OutputMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// `content`：显示匹配行（默认）。
    Default,
    /// `files_with_matches`：只列文件名。
    FilesOnly,
    /// `count`：每文件匹配计数。
    CountOnly,
    /// `files_without_matches`：列无匹配文件。
    FilesWithoutMatch,
}

impl OutputMode {
    /// 从 `output_mode` 字符串解析；未知值给出源错误文案。
    pub fn parse(raw: &str) -> Result<Self, GrepArgError> {
        match raw {
            "content" => Ok(Self::Default),
            "files_with_matches" => Ok(Self::FilesOnly),
            "count" => Ok(Self::CountOnly),
            "files_without_matches" => Ok(Self::FilesWithoutMatch),
            other => Err(GrepArgError::new(format!(
                "Invalid output_mode: '{other}'. Must be 'content', 'files_with_matches', 'count', or 'files_without_matches'"
            ))),
        }
    }
}

/// `type` 参数 → glob 模式列表（源：`grep_args.rs::type_to_glob`，逐项保留）。
pub fn type_to_glob(type_name: &str) -> Vec<&'static str> {
    match type_name {
        "rust" => vec!["*.rs"],
        "js" => vec!["*.js", "*.mjs"],
        "py" => vec!["*.py"],
        "go" => vec!["*.go"],
        "java" => vec!["*.java"],
        "ts" => vec!["*.ts", "*.tsx"],
        "c" => vec!["*.c", "*.h"],
        "cpp" => vec!["*.cpp", "*.hpp", "*.cc", "*.cxx"],
        "ruby" | "rb" => vec!["*.rb"],
        "swift" => vec!["*.swift"],
        "kotlin" | "kt" => vec!["*.kt", "*.kts"],
        "scala" => vec!["*.scala"],
        "html" => vec!["*.html", "*.htm"],
        "css" => vec!["*.css", "*.scss", "*.sass", "*.less"],
        "json" => vec!["*.json"],
        "yaml" | "yml" => vec!["*.yaml", "*.yml"],
        "markdown" | "md" => vec!["*.md", "*.mdx"],
        "sql" => vec!["*.sql"],
        "shell" | "sh" => vec!["*.sh", "*.bash", "*.zsh"],
        _ => vec![],
    }
}

/// Grep 工具的结构化输入（迁移自源 `GrepInput`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepInput {
    /// 正则或字面量模式（`fixed_strings` 决定）。
    pub pattern: String,
    /// 搜索路径；`None` = 调用方 cwd。
    pub path: Option<String>,
    /// `glob` 过滤器（单模式，支持 `*.{ts,tsx}` 花括号）。
    pub glob: Option<String>,
    /// `type` 过滤器（映射为 glob）。
    pub type_filter: Option<String>,
    /// 输出模式字符串（`None` = 默认 content）。
    pub output_mode: Option<String>,
    /// 大小写不敏感（`case_insensitive` 语义别名优先于 `-i`）。
    pub case_insensitive: bool,
    /// 对称上下文行数（`context` 优先于 `-C`）。
    pub context: Option<usize>,
    /// 匹配前上下文（`before_context` 优先于 `-B`）。
    pub before_context: Option<usize>,
    /// 匹配后上下文（`after_context` 优先于 `-A`）。
    pub after_context: Option<usize>,
    /// 是否显示行号（`show_line_numbers` 优先于 `-n`，默认 true）。
    pub line_number: bool,
    /// 多行模式（`^`/`$` 行边界，`.` 匹配换行）。
    pub multiline: bool,
    /// 整词匹配。
    pub whole_word: bool,
    /// 反转匹配。
    pub invert_match: bool,
    /// 固定字面量模式。
    pub fixed_strings: bool,
    /// 输出行数上限（`0` = unlimited，默认 250）。
    pub head_limit: usize,
    /// 跳过最终输出的前 N 行（在截断之后应用）。
    pub offset: Option<usize>,
    /// 遍历深度上限。
    pub max_depth: Option<usize>,
}

/// 别名解析：先语义键，再 CLI 键（源 `or_else` 顺序）。
///
/// 语义键存在但值不是布尔 → 取默认值，**不**回落到 CLI 键（源行为）。
fn bool_alias(args: &Value, semantic: &str, cli: &str, default: bool) -> bool {
    args.get(semantic)
        .or_else(|| args.get(cli))
        .and_then(|value| value.as_bool())
        .unwrap_or(default)
}

/// 非负整数参数解析（源：`tools::parse_optional_u64`）。
///
/// - `null`/缺失 → `Ok(None)`
/// - 非数值 → `Error: '{name}' must be a non-negative integer, got {原始值}`
/// - 小数或负数 → `Error: '{name}' must be a non-negative integer, got {n}`
pub fn parse_optional_u64(value: &Value, name: &str) -> Result<Option<u64>, GrepArgError> {
    if value.is_null() {
        return Ok(None);
    }
    let n = value.as_f64().ok_or_else(|| {
        GrepArgError::new(format!(
            "Error: '{name}' must be a non-negative integer, got {value}"
        ))
    })?;
    if n.fract() != 0.0 || n < 0.0 {
        return Err(GrepArgError::new(format!(
            "Error: '{name}' must be a non-negative integer, got {n}"
        )));
    }
    Ok(Some(n as u64))
}

/// 数值别名解析：语义键优先，缺省则 CLI 键。
fn number_alias(args: &Value, semantic: &str, cli: &str) -> Result<Option<u64>, GrepArgError> {
    let value = args
        .get(semantic)
        .or_else(|| args.get(cli))
        .unwrap_or(&Value::Null);
    parse_optional_u64(value, semantic)
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

impl GrepInput {
    /// 从 `tools/call` 的 `arguments` 对象解析输入。
    ///
    /// 缺少 `pattern` 时给出源错误文案
    /// `Error: Missing required parameter 'pattern'`。
    pub fn from_arguments(args: &Value) -> Result<Self, GrepArgError> {
        let pattern = match args.get("pattern").and_then(|value| value.as_str()) {
            Some(pattern) => pattern.to_string(),
            None => {
                return Err(GrepArgError::new(
                    "Error: Missing required parameter 'pattern'",
                ))
            }
        };

        let head_limit =
            match parse_optional_u64(args.get("head_limit").unwrap_or(&Value::Null), "head_limit")?
            {
                Some(n) => n as usize,
                None => DEFAULT_HEAD_LIMIT,
            };

        Ok(Self {
            pattern,
            path: optional_string(args, "path"),
            glob: optional_string(args, "glob"),
            type_filter: optional_string(args, "type"),
            output_mode: optional_string(args, "output_mode"),
            case_insensitive: bool_alias(args, "case_insensitive", "-i", false),
            context: number_alias(args, "context", "-C")?.map(|n| n as usize),
            before_context: number_alias(args, "before_context", "-B")?.map(|n| n as usize),
            after_context: number_alias(args, "after_context", "-A")?.map(|n| n as usize),
            line_number: bool_alias(args, "show_line_numbers", "-n", true),
            multiline: args
                .get("multiline")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            whole_word: args
                .get("whole_word")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            invert_match: args
                .get("invert_match")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            fixed_strings: args
                .get("fixed_strings")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            head_limit,
            offset: parse_optional_u64(args.get("offset").unwrap_or(&Value::Null), "offset")?
                .map(|n| n as usize),
            max_depth: parse_optional_u64(
                args.get("max_depth").unwrap_or(&Value::Null),
                "max_depth",
            )?
            .map(|n| n as usize),
        })
    }

    /// 转译为搜索引擎参数（源：`GrepInput::to_parsed_args`）。
    pub fn to_parsed_args(&self) -> Result<ParsedArgs, GrepArgError> {
        let mode_str = self.output_mode.as_deref().unwrap_or("content");
        let output_mode = OutputMode::parse(mode_str)?;

        // glob + type 映射共用同一过滤器列表（源按顺序 push）。
        let mut glob_filters = Vec::new();
        if let Some(glob) = &self.glob {
            glob_filters.push(glob.clone());
        }
        if let Some(type_name) = &self.type_filter {
            for glob in type_to_glob(type_name) {
                glob_filters.push(glob.to_string());
            }
        }

        // `-C` 是对称上下文简写；`-A`/`-B` 任一出现即按非对称处理（源语义）。
        let (before, after) = if self.before_context.is_some() || self.after_context.is_some() {
            (
                self.before_context.unwrap_or(0),
                self.after_context.unwrap_or(0),
            )
        } else {
            let c = self.context.unwrap_or(0);
            (c, c)
        };

        Ok(ParsedArgs {
            pattern: self.pattern.clone(),
            path: self.path.clone(),
            glob_filters,
            output_mode,
            before_context: before,
            after_context: after,
            case_insensitive: self.case_insensitive,
            whole_word: self.whole_word,
            multiline: self.multiline,
            line_number: self.line_number,
            invert_match: self.invert_match,
            fixed_strings: self.fixed_strings,
            max_depth: self.max_depth,
        })
    }
}

/// 搜索引擎参数（迁移自源 `ParsedArgs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArgs {
    /// 模式。
    pub pattern: String,
    /// 搜索路径；`None` = cwd。
    pub path: Option<String>,
    /// glob 过滤器（已含 `type` 映射结果）。
    pub glob_filters: Vec<String>,
    /// 输出模式。
    pub output_mode: OutputMode,
    /// 匹配前上下文行数。
    pub before_context: usize,
    /// 匹配后上下文行数。
    pub after_context: usize,
    /// 大小写不敏感。
    pub case_insensitive: bool,
    /// 整词匹配。
    pub whole_word: bool,
    /// 多行模式。
    pub multiline: bool,
    /// 显示行号。
    pub line_number: bool,
    /// 反转匹配。
    pub invert_match: bool,
    /// 固定字面量。
    pub fixed_strings: bool,
    /// 深度上限。
    pub max_depth: Option<usize>,
}
