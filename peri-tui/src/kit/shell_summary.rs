//! Shell 命令的纯工具链摘要。
//!
//! 只解析命令文本，不执行、不展开变量。目标是让工具卡头行回答"用了哪些工具"，
//! 完整命令由卡片第二行常驻展示。解析失败时回退到命令首行，保留可读性。

/// 头行最多展示的工具段数，超出部分折叠为 `… +N`。
const MAX_CHAIN_SEGMENTS: usize = 4;
/// 单个工具段的最大字符数（含子命令）。
const MAX_TOOL_CHARS: usize = 48;
/// 完整命令的展示上限；异常长的 heredoc 不把整段正文带进卡片。
pub(crate) const MAX_COMMAND_BYTES: usize = 32 * 1024;
/// 回退摘要的最大字符数。
const MAX_FALLBACK_CHARS: usize = 80;
/// 卡片第二行最多常驻的命令行数。
pub(crate) const MAX_COMMAND_LINES: usize = 6;

/// Shell 卡片的展示数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellDisplay {
    /// 纯工具链，例如 `grep | head`、`cargo test | tail`。
    pub(crate) tool_chain: String,
    /// 完整原始命令（受 [`MAX_COMMAND_BYTES`] 约束）。
    pub(crate) command: String,
}

/// 从 Bash 工具输入生成展示数据；空命令返回 `None`。
pub(crate) fn display_for(command: &str) -> Option<ShellDisplay> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let stages = split_stages(command);
    let tool_chain = stages
        .map(|stages| tool_chain(&stages, command))
        .unwrap_or_else(|| fallback_label(command));
    Some(ShellDisplay {
        tool_chain,
        command: truncate_bytes(command, MAX_COMMAND_BYTES),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Connector {
    First,
    Pipe,
    Sequence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stage {
    connector: Connector,
    tokens: Vec<String>,
}

/// 按 shell 控制符切分阶段。引号、转义、`$(...)` 内的控制符不切分。
///
/// heredoc 在 `<<` 处停止，避免把正文当作命令；未闭合引号 / 命令替换返回 `None`。
fn split_stages(command: &str) -> Option<Vec<Stage>> {
    let mut stages = Vec::new();
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut connector = Connector::First;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut subst_depth = 0usize;
    let mut backtick = false;
    let mut chars = command.chars().peekable();

    while let Some(c) = chars.next() {
        if escaped {
            if c == '\n' {
                escaped = false;
                continue;
            }
            token.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
            continue;
        }
        if backtick {
            if c == '`' {
                backtick = false;
            } else {
                token.push(c);
            }
            continue;
        }

        match c {
            '\'' | '"' => quote = Some(c),
            '`' => backtick = true,
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                subst_depth += 1;
                token.push_str("$(");
            }
            '(' if subst_depth > 0 => {
                subst_depth += 1;
                token.push(c);
            }
            ')' if subst_depth > 0 => {
                subst_depth -= 1;
                token.push(c);
            }
            ' ' | '\t' | '\r' if subst_depth == 0 => {
                flush_token(&mut token, &mut tokens);
            }
            '\n' if subst_depth == 0 => {
                flush_token(&mut token, &mut tokens);
                flush_stage(&mut stages, &mut tokens, connector);
                connector = Connector::Sequence;
            }
            '#' if subst_depth == 0 && token.is_empty() => {
                flush_token(&mut token, &mut tokens);
                flush_stage(&mut stages, &mut tokens, connector);
                connector = Connector::Sequence;
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '<' if subst_depth == 0 && chars.peek() == Some(&'<') => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    // here-string：`<<<` 不是 heredoc，继续解析。
                    chars.next();
                    token.push_str("<<<");
                } else {
                    // heredoc：正文不参与工具链解析。
                    flush_token(&mut token, &mut tokens);
                    flush_stage(&mut stages, &mut tokens, connector);
                    return Some(stages);
                }
            }
            '>' if subst_depth == 0 => {
                token.push('>');
                match chars.peek().copied() {
                    Some('>') => {
                        chars.next();
                        token.push('>');
                    }
                    Some('&') => {
                        chars.next();
                        token.push('&');
                    }
                    _ => {}
                }
            }
            '<' if subst_depth == 0 => {
                token.push('<');
                if chars.peek() == Some(&'&') {
                    chars.next();
                    token.push('&');
                }
            }
            '&' if subst_depth == 0 => {
                if chars.peek() == Some(&'>') {
                    chars.next();
                    token.push_str("&>");
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        token.push('>');
                    }
                } else {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                    }
                    flush_token(&mut token, &mut tokens);
                    flush_stage(&mut stages, &mut tokens, connector);
                    connector = Connector::Sequence;
                }
            }
            '|' if subst_depth == 0 => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                    flush_token(&mut token, &mut tokens);
                    flush_stage(&mut stages, &mut tokens, connector);
                    connector = Connector::Sequence;
                } else {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                    }
                    flush_token(&mut token, &mut tokens);
                    flush_stage(&mut stages, &mut tokens, connector);
                    connector = Connector::Pipe;
                }
            }
            ';' if subst_depth == 0 => {
                flush_token(&mut token, &mut tokens);
                flush_stage(&mut stages, &mut tokens, connector);
                connector = Connector::Sequence;
            }
            _ => token.push(c),
        }
    }

    if quote.is_some() || backtick || subst_depth > 0 {
        return None;
    }
    flush_token(&mut token, &mut tokens);
    flush_stage(&mut stages, &mut tokens, connector);
    Some(stages)
}

fn flush_token(token: &mut String, tokens: &mut Vec<String>) {
    if !token.is_empty() {
        tokens.push(std::mem::take(token));
    }
}

fn flush_stage(stages: &mut Vec<Stage>, tokens: &mut Vec<String>, connector: Connector) {
    if !tokens.is_empty() {
        stages.push(Stage {
            connector,
            tokens: std::mem::take(tokens),
        });
    }
}

fn tool_chain(stages: &[Stage], command: &str) -> String {
    if stages
        .iter()
        .filter_map(|stage| stage_first_word(&stage.tokens))
        .any(is_complex_keyword)
    {
        return fallback_label(command);
    }

    let mut out = String::new();
    let mut shown = 0usize;
    let mut total = 0usize;
    for stage in stages {
        let Some(tool) = tool_of_stage(&stage.tokens) else {
            continue;
        };
        total += 1;
        if shown >= MAX_CHAIN_SEGMENTS {
            continue;
        }
        if shown > 0 {
            out.push_str(match stage.connector {
                Connector::Pipe => " | ",
                Connector::Sequence | Connector::First => " ; ",
            });
        }
        out.push_str(&tool);
        shown += 1;
    }

    if total > shown {
        out.push_str(&format!(" … +{}", total - shown));
    }
    if out.is_empty() {
        fallback_label(command)
    } else {
        out
    }
}

fn stage_first_word(tokens: &[String]) -> Option<&str> {
    tokens
        .iter()
        .find(|token| !is_assignment(token) && !is_redirection(token))
        .map(String::as_str)
}

fn tool_of_stage(tokens: &[String]) -> Option<String> {
    let mut index = 0usize;
    loop {
        while index < tokens.len() && is_redirection(&tokens[index]) {
            let bare = is_bare_redirection(&tokens[index]);
            index += 1;
            if bare && index < tokens.len() && !is_redirection(&tokens[index]) {
                index += 1;
            }
        }
        if index >= tokens.len() {
            return None;
        }
        if is_assignment(&tokens[index]) {
            index += 1;
            continue;
        }
        let word = tokens[index].as_str();
        if is_skip_stage(word) {
            return None;
        }
        if let Some(next) = wrapper_next(word, tokens, index) {
            index = next;
            continue;
        }
        break;
    }

    let program = basename(&tokens[index]);
    if program.is_empty() {
        return None;
    }

    let mut tool = if program == "[" {
        "test".to_string()
    } else {
        program.to_string()
    };
    if program == "xargs" {
        if let Some(sub) = xargs_program(tokens, index + 1) {
            tool.push(' ');
            tool.push_str(&sub);
        }
    } else if SUBCOMMAND_PROGRAMS.contains(&program)
        && let Some(sub) = subcommand(tokens, index + 1, program)
    {
        tool.push(' ');
        tool.push_str(&sub);
    }
    Some(truncate_chars(&tool, MAX_TOOL_CHARS))
}

fn basename(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

fn is_complex_keyword(word: &str) -> bool {
    if word.starts_with('(') || word.starts_with('{') || word == "}" || word == ")" {
        return true;
    }
    matches!(
        word,
        "if" | "then"
            | "elif"
            | "else"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "select"
            | "function"
            | "coproc"
            | "[["
    )
}

fn is_skip_stage(word: &str) -> bool {
    matches!(
        word,
        "cd" | "pushd"
            | "popd"
            | "export"
            | "unset"
            | "set"
            | "source"
            | "."
            | "alias"
            | "shopt"
            | "umask"
            | "ulimit"
            | "declare"
            | "local"
            | "readonly"
            | "typeset"
    )
}

/// 包装器：跳过自身后继续寻找真正执行的程序。返回下一个待检查的 token 下标。
fn wrapper_next(word: &str, tokens: &[String], index: usize) -> Option<usize> {
    match word {
        "!" => Some(index + 1),
        "sudo" | "env" | "time" | "nohup" | "exec" | "stdbuf" | "nice" => {
            Some(skip_options(word, tokens, index + 1))
        }
        "timeout" => {
            let mut next = skip_options(word, tokens, index + 1);
            if next < tokens.len() {
                next += 1; // duration
            }
            Some(next)
        }
        _ => None,
    }
}

fn skip_options(program: &str, tokens: &[String], mut index: usize) -> usize {
    while index < tokens.len() && tokens[index].starts_with('-') {
        if tokens[index].contains('=') {
            index += 1;
        } else if option_takes_value(program, &tokens[index]) {
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

fn subcommand(tokens: &[String], mut index: usize, program: &str) -> Option<String> {
    while index < tokens.len() {
        let token = &tokens[index];
        if token == "--" {
            index += 1;
            continue;
        }
        if token.starts_with('-') || token.starts_with('+') {
            if token.contains('=') {
                index += 1;
            } else if option_takes_value(program, token) {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if is_redirection(token) {
            return None;
        }
        if is_assignment(token) {
            index += 1;
            continue;
        }
        return Some(truncate_chars(token, MAX_TOOL_CHARS));
    }
    None
}

fn xargs_program(tokens: &[String], mut index: usize) -> Option<String> {
    while index < tokens.len() {
        let token = &tokens[index];
        if token.starts_with('-') {
            if token.contains('=') {
                index += 1;
            } else if option_takes_value("xargs", token) {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        let program = basename(token);
        if program.is_empty() {
            return None;
        }
        let mut tool = program.to_string();
        if SUBCOMMAND_PROGRAMS.contains(&program)
            && let Some(sub) = subcommand(tokens, index + 1, program)
        {
            tool.push(' ');
            tool.push_str(&sub);
        }
        return Some(truncate_chars(&tool, MAX_TOOL_CHARS));
    }
    None
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_redirection(token: &str) -> bool {
    let token = token.trim_start_matches(|c: char| c.is_ascii_digit());
    token.starts_with('>') || token.starts_with('<') || token.starts_with("&>")
}

fn is_bare_redirection(token: &str) -> bool {
    let token = token.trim_start_matches(|c: char| c.is_ascii_digit());
    matches!(token, ">" | ">>" | "<" | "<<<" | "&>" | "&>>")
}

fn fallback_label(command: &str) -> String {
    let first = command
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    truncate_chars(first, MAX_FALLBACK_CHARS)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut out: String = value.chars().take(limit).collect();
    out.push('…');
    out
}

fn truncate_bytes(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

/// 常见支持子命令的 CLI；子命令比裸程序名更有信息量（`git` → `git status`）。
const SUBCOMMAND_PROGRAMS: &[&str] = &[
    "git",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "go",
    "rustup",
    "gh",
    "just",
    "make",
    "uv",
    "pip",
    "pip3",
    "poetry",
    "terraform",
    "aws",
    "az",
    "gcloud",
    "brew",
    "nix",
];

fn option_takes_value(program: &str, option: &str) -> bool {
    let flag = option.split('=').next().unwrap_or(option);
    match program {
        "git" => matches!(
            flag,
            "-C" | "-c"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--exec-path"
                | "--config-env"
                | "--super-prefix"
        ),
        "cargo" => matches!(
            flag,
            "--manifest-path"
                | "--config"
                | "-Z"
                | "--target-dir"
                | "--target"
                | "--features"
                | "-F"
                | "--package"
                | "-p"
                | "--profile"
                | "--jobs"
                | "-j"
                | "--color"
                | "--message-format"
        ),
        "npm" | "pnpm" | "yarn" | "bun" => matches!(
            flag,
            "--prefix"
                | "--cwd"
                | "-C"
                | "--filter"
                | "-F"
                | "--workspace"
                | "-w"
                | "--registry"
                | "--userconfig"
                | "--cache"
        ),
        "docker" | "podman" => matches!(
            flag,
            "--context"
                | "-H"
                | "--host"
                | "--config"
                | "--log-level"
                | "--tlscacert"
                | "--tlscert"
                | "--tlskey"
        ),
        "kubectl" => matches!(
            flag,
            "--context"
                | "--namespace"
                | "-n"
                | "--kubeconfig"
                | "--cluster"
                | "--user"
                | "--token"
        ),
        "helm" => matches!(
            flag,
            "--kube-context"
                | "--namespace"
                | "-n"
                | "--kubeconfig"
                | "--registry-config"
                | "--repository-config"
        ),
        "go" => matches!(
            flag,
            "-C" | "-modfile" | "--modfile" | "-mod" | "--mod" | "-p" | "--p" | "-o" | "--o"
        ),
        "gh" => matches!(
            flag,
            "--repo" | "-R" | "--hostname" | "--jq" | "-q" | "--template" | "-t"
        ),
        "sudo" => matches!(
            flag,
            "-u" | "-g" | "-p" | "-C" | "-h" | "-U" | "-r" | "-t" | "-T"
        ),
        "env" => matches!(
            flag,
            "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
        ),
        "time" => matches!(flag, "-f" | "--format" | "-o" | "--output"),
        "timeout" => matches!(flag, "-s" | "--signal" | "-k" | "--kill-after"),
        "nice" => matches!(flag, "-n" | "--adjustment"),
        "stdbuf" => matches!(
            flag,
            "-i" | "-o" | "-e" | "--input" | "--output" | "--error"
        ),
        "xargs" => matches!(
            flag,
            "-a" | "--arg-file"
                | "-E"
                | "-e"
                | "-I"
                | "-i"
                | "-L"
                | "-l"
                | "-n"
                | "-P"
                | "-s"
                | "--max-args"
                | "--max-procs"
                | "--delimiter"
                | "-d"
                | "--replace"
        ),
        _ => false,
    }
}

#[cfg(test)]
#[path = "shell_summary_test.rs"]
mod tests;
