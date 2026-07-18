/// 从 args 数组中解析搜索参数
pub(crate) struct ParsedArgs {
    pub(crate) pattern: String,
    pub(crate) path: Option<String>,        // 搜索路径，None 表示 cwd
    pub(crate) glob_filters: Vec<String>,   // -g 参数
    pub(crate) _type_filters: Vec<String>,  // -t 参数（暂不实现）
    pub(crate) _type_excludes: Vec<String>, // -T 参数（暂不实现）
    pub(crate) output_mode: OutputMode,     // 默认/文件名/计数/无匹配文件
    pub(crate) before_context: usize,       // -B 参数
    pub(crate) after_context: usize,        // -A 参数
    pub(crate) case_insensitive: bool,      // -i 参数
    pub(crate) whole_word: bool,            // -w 参数
    pub(crate) multiline: bool,             // 多行模式
    pub(crate) line_number: bool,           // 显示行号
    pub(crate) invert_match: bool,          // -v 反转匹配
    pub(crate) fixed_strings: bool,         // -F 固定字符串
    pub(crate) max_depth: Option<usize>,    // 搜索深度限制
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum OutputMode {
    Default,           // 显示匹配行
    FilesOnly,         // -l
    CountOnly,         // -c
    FilesWithoutMatch, // -L
}

/// Grep 工具的结构化输入参数，从 JSON 直接反序列化
pub(crate) struct GrepInput {
    pub(crate) pattern: String,
    pub(crate) path: Option<String>,
    pub(crate) glob: Option<String>,
    pub(crate) type_filter: Option<String>,
    pub(crate) output_mode: Option<String>, // "content" | "files_with_matches" | "count" | "files_without_matches"
    pub(crate) case_insensitive: bool,      // 对应 -i，默认 false
    pub(crate) context: Option<usize>,      // 对应 -C
    pub(crate) before_context: Option<usize>, // 对应 -B
    pub(crate) after_context: Option<usize>, // 对应 -A
    pub(crate) line_number: bool,           // 对应 -n，默认 true
    pub(crate) multiline: bool,             // 多行模式，默认 false
    pub(crate) whole_word: bool,            // -w，默认 false
    pub(crate) invert_match: bool,          // -v，默认 false
    pub(crate) fixed_strings: bool,         // -F，默认 false
    pub(crate) head_limit: usize,           // 默认 250
    pub(crate) offset: Option<usize>,       // 跳过前 N 行
    pub(crate) max_depth: Option<usize>,    // 搜索深度限制
}

/// 将 type 参数（如 "rust"、"js"）映射为 glob 模式列表
pub(crate) fn type_to_glob(type_name: &str) -> Vec<&'static str> {
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

impl GrepInput {
    /// 将结构化参数转译为搜索引擎所需的 ParsedArgs
    pub(crate) fn to_parsed_args(&self) -> Result<ParsedArgs, String> {
        // output_mode 字符串 → OutputMode 枚举（默认 "content"）
        let mode_str = self.output_mode.as_deref().unwrap_or("content");
        let output_mode = match mode_str {
            "content" => OutputMode::Default,
            "files_with_matches" => OutputMode::FilesOnly,
            "count" => OutputMode::CountOnly,
            "files_without_matches" => OutputMode::FilesWithoutMatch,
            other => {
                return Err(format!(
                    "Invalid output_mode: '{}'. Must be 'content', 'files_with_matches', 'count', or 'files_without_matches'",
                    other
                ));
            }
        };

        // 组装 glob 过滤器：用户提供的 glob + type 映射
        let mut glob_filters = Vec::new();
        if let Some(ref glob) = self.glob {
            // 支持多 glob 模式，如 "*.{ts,tsx}" 或 "*.rs"
            glob_filters.push(glob.clone());
        }
        if let Some(ref type_name) = self.type_filter {
            let type_globs = type_to_glob(type_name);
            for g in type_globs {
                glob_filters.push(g.to_string());
            }
        }

        // -C 作为对称上下文的简写，-A/-B 优先
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
            _type_filters: vec![],
            _type_excludes: vec![],
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
