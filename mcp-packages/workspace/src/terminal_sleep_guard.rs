//! Foreground `sleep` guard for the synchronous Bash path.
//!
//! Models frequently wait for builds/tests/CI with direct `sleep 100`-style
//! commands. Those waits occupy a foreground call and hide whether the real
//! work is progressing, so `BashTool` rejects them and points callers at
//! `run_in_background: true`.
//!
//! [`foreground_sleep_seconds`] is a heuristic scan of the common direct forms,
//! not a shell parser or a security sandbox. Wrapper forms that hide the
//! command from the scan (for example `bash -c 'sleep 115'`), aliases, and
//! sleeps inside scripts are allowed to pass. Durations that cannot be resolved
//! statically (for example `sleep "$N"`) are ignored. That is intentional: the
//! guard targets the high-volume direct pattern without rejecting commands it
//! cannot understand.

/// Foreground sleeps at or above this duration are rejected.
pub(super) const MAX_FOREGROUND_SLEEP_SECONDS: f64 = 60.0;

/// Longest `sleep` duration requested by any simple command in `command`.
///
/// Returns `None` when no statically parseable, foreground `sleep` is found.
/// A segment backgrounded with `&` does not block the caller and is ignored.
pub(super) fn foreground_sleep_seconds(command: &str) -> Option<f64> {
    simple_command_segments(command)
        .iter()
        .filter_map(|tokens| sleep_seconds_in_segment(tokens))
        .reduce(f64::max)
}

/// Split a shell command into simple-command token lists.
///
/// Quotes and comments are not scanned. `;`, newlines, pipes, `&&`, `||`,
/// subshell/group delimiters, and reserved words all start a new segment;
/// segments backgrounded with a single `&` are dropped because they do not
/// block the caller.
fn simple_command_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                for quoted in chars.by_ref() {
                    if quoted == '\'' {
                        break;
                    }
                    current.push(quoted);
                }
            }
            '"' => {
                while let Some(quoted) = chars.next() {
                    match quoted {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                current.push(escaped);
                            }
                        }
                        _ => current.push(quoted),
                    }
                }
            }
            '\\' => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '#' if current.is_empty() => {
                for commented in chars.by_ref() {
                    if commented == '\n' {
                        break;
                    }
                }
                finish_segment(&mut segments, &mut tokens, &mut current, false);
            }
            '\n' | ';' | '(' | ')' | '{' | '}' => {
                finish_segment(&mut segments, &mut tokens, &mut current, false);
            }
            '|' => {
                if matches!(chars.peek(), Some(&'|') | Some(&'&')) {
                    chars.next();
                }
                finish_segment(&mut segments, &mut tokens, &mut current, false);
            }
            '&' => match chars.peek() {
                Some(&'>') => {
                    chars.next();
                    if chars.peek() == Some(&'>') {
                        chars.next();
                    }
                    flush_token(&mut tokens, &mut current);
                    tokens.push("&>".to_string());
                }
                Some(&'&') => {
                    chars.next();
                    finish_segment(&mut segments, &mut tokens, &mut current, false);
                }
                _ => finish_segment(&mut segments, &mut tokens, &mut current, true),
            },
            '<' | '>' => {
                // A leading all-digit word directly before the operator is an
                // fd number (`2>`), not a command argument.
                if current.chars().all(|ch| ch.is_ascii_digit()) {
                    current.clear();
                } else {
                    flush_token(&mut tokens, &mut current);
                }
                if chars.peek() == Some(&ch) {
                    chars.next();
                }
                tokens.push(ch.to_string());
                if chars.peek() == Some(&'&') {
                    chars.next();
                    while matches!(chars.peek(), Some(next) if next.is_ascii_digit()) {
                        chars.next();
                    }
                }
            }
            '!' if current.is_empty() && chars.peek().is_none_or(|next| next.is_whitespace()) => {
                tokens.push("!".to_string());
            }
            whitespace if whitespace.is_whitespace() => {
                flush_token(&mut tokens, &mut current);
            }
            other => current.push(other),
        }
    }

    finish_segment(&mut segments, &mut tokens, &mut current, false);
    segments
}

fn flush_token(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

fn finish_segment(
    segments: &mut Vec<Vec<String>>,
    tokens: &mut Vec<String>,
    current: &mut String,
    backgrounded: bool,
) {
    flush_token(tokens, current);
    if backgrounded || tokens.is_empty() {
        tokens.clear();
    } else {
        segments.push(std::mem::take(tokens));
    }
}

/// Total statically known duration requested by a `sleep` simple command.
fn sleep_seconds_in_segment(tokens: &[String]) -> Option<f64> {
    let command_index = skip_command_prefixes(tokens);
    if tokens.get(command_index).map(String::as_str) != Some("sleep") {
        return None;
    }

    let mut total = 0.0;
    let mut parsed_any = false;
    for token in &tokens[command_index + 1..] {
        if token.contains('<') || token.contains('>') {
            break;
        }
        if token == "--" {
            continue;
        }
        if token.starts_with('-') {
            // `--help`/`--version` do not sleep; do not guess at other options.
            return None;
        }
        total += parse_sleep_duration(token)?;
        parsed_any = true;
    }
    parsed_any.then_some(total)
}

/// Skip `then`/`do`/`else` keywords, negation, variable assignments, and the
/// common wrapper prefixes that still execute their trailing command.
fn skip_command_prefixes(tokens: &[String]) -> usize {
    let mut index = 0;
    loop {
        let Some(token) = tokens.get(index).map(String::as_str) else {
            return index;
        };
        match token {
            "then" | "do" | "else" | "elif" | "!" => index += 1,
            "sudo" => {
                index = skip_options(
                    tokens,
                    index + 1,
                    &[
                        "-u",
                        "--user",
                        "-g",
                        "--group",
                        "-p",
                        "--prompt",
                        "-C",
                        "--close-from",
                        "-h",
                        "--host",
                        "-r",
                        "--role",
                        "-t",
                        "--type",
                        "-T",
                        "--command-timeout",
                        "-U",
                        "--other-user",
                        "-D",
                        "--chdir",
                        "-R",
                        "--chroot",
                    ],
                );
            }
            "time" | "nohup" | "command" => {
                index = skip_options(tokens, index + 1, &[]);
            }
            "exec" => index = skip_options(tokens, index + 1, &["-a"]),
            "env" => {
                index = skip_options(
                    tokens,
                    index + 1,
                    &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"],
                );
                while tokens
                    .get(index)
                    .is_some_and(|token| is_env_assignment(token))
                {
                    index += 1;
                }
            }
            _ if is_env_assignment(token) => index += 1,
            _ => return index,
        }
    }
}

fn skip_options(tokens: &[String], mut index: usize, value_options: &[&str]) -> usize {
    while let Some(token) = tokens.get(index) {
        if token == "--" {
            return index + 1;
        }
        if !token.starts_with('-') || token == "-" {
            break;
        }
        let takes_value = value_options.contains(&token.as_str());
        index += 1;
        if takes_value && index < tokens.len() {
            index += 1;
        }
    }
    index
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Parse `N`, `1.5`, `2m`, `1h`, `30s`, `1d`, `inf`, ... into seconds.
fn parse_sleep_duration(token: &str) -> Option<f64> {
    if token.eq_ignore_ascii_case("inf") || token.eq_ignore_ascii_case("infinity") {
        return Some(f64::INFINITY);
    }
    let (number, multiplier) = match token.as_bytes().last().copied()? {
        b's' | b'S' => (&token[..token.len() - 1], 1.0),
        b'm' | b'M' => (&token[..token.len() - 1], 60.0),
        b'h' | b'H' => (&token[..token.len() - 1], 3600.0),
        b'd' | b'D' => (&token[..token.len() - 1], 86400.0),
        _ => (token, 1.0),
    };
    let value: f64 = number.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value * multiplier)
}
