//! 仅识别静态命令位置的 sleep；不是 shell 解释器或安全沙箱。
//! 跳过引号、注释、here-doc、bash/sh -c 字符串，不展开变量、别名或动态脚本。
use serde::Serialize;

pub(crate) const THRESHOLD_MS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellPlatform {
    Unix,
    Windows,
}

impl ShellPlatform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SleepRejection {
    pub(crate) code: &'static str,
    started: bool,
    threshold_ms: u64,
    detected_ms: u64,
    segment: String,
    alternatives: Vec<&'static str>,
}

impl SleepRejection {
    pub(crate) fn text(&self) -> String {
        format!(
            "code: {}\nstarted: false\n检测到长 sleep：{}ms；门槛 {}ms（60s）。\n{}",
            self.code,
            self.detected_ms,
            self.threshold_ms,
            self.alternatives.join("\n")
        )
    }
}

#[derive(Default)]
struct Segment {
    words: Vec<String>,
    detached: bool,
}

// 引号整体变成不可识别的参数，避免 echo/grep 文本被当作命令；保留空参数位置。
fn segments(command: &str) -> Vec<Segment> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut segment = Segment::default();
    let mut word = String::new();
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            let quote = c;
            word.push('\0');
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && quote == '"' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '\\' {
            word.push('\0');
            i += 2;
            continue;
        }
        if c == '#' && word.is_empty() {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '<' && chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') {
            if !word.is_empty() {
                segment.words.push(std::mem::take(&mut word));
            }
            i += 2;
            let strip_tabs = chars.get(i) == Some(&'-');
            if strip_tabs {
                i += 1;
            }
            while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
                i += 1;
            }
            let mut delimiter = String::new();
            let mut quote = None;
            while i < chars.len() {
                let c = chars[i];
                if quote == Some(c) {
                    quote = None;
                    i += 1;
                    continue;
                }
                if quote.is_none() && (c == '\'' || c == '"') {
                    quote = Some(c);
                    i += 1;
                    continue;
                }
                if quote.is_none() && (c.is_whitespace() || ";&|".contains(c)) {
                    break;
                }
                delimiter.push(c);
                i += 1;
            }
            heredocs.push((delimiter, strip_tabs));
            continue;
        }
        let substitution = c == '$' && chars.get(i + 1) == Some(&'(');
        if c.is_whitespace() || ";&|(){} `".contains(c) || substitution {
            if !word.is_empty() {
                segment.words.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() || c == '\n' {
                segment.detached = c == '&' && chars.get(i + 1) != Some(&'&');
                if !segment.words.is_empty() {
                    out.push(std::mem::take(&mut segment));
                }
                if matches!(c, '&' | '|') && chars.get(i + 1) == Some(&c) {
                    i += 1;
                }
            }
            i += if substitution { 2 } else { 1 };
            if c == '\n' && !heredocs.is_empty() {
                for (delimiter, strip_tabs) in std::mem::take(&mut heredocs) {
                    while i < chars.len() {
                        let start = i;
                        while i < chars.len() && chars[i] != '\n' {
                            i += 1;
                        }
                        let line: String = chars[start..i].iter().collect();
                        if i < chars.len() {
                            i += 1;
                        }
                        if (if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        }) == delimiter
                        {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        word.push(c);
        i += 1;
    }
    if !word.is_empty() {
        segment.words.push(word);
    }
    if !segment.words.is_empty() {
        out.push(segment);
    }
    out
}

fn command_index(words: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < words.len() {
        match words[i].as_str() {
            "command" | "exec" | "nohup" => {
                i += 1;
                while words.get(i).is_some_and(|s| s.starts_with('-')) {
                    i += 1;
                }
            }
            "env" => {
                i += 1;
                while i < words.len() {
                    if words[i] == "-u" || words[i] == "--unset" {
                        i += 2;
                    } else if words[i].starts_with('-') || words[i].contains('=') {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            "timeout" => {
                i += 1;
                while words.get(i).is_some_and(|s| s.starts_with('-')) {
                    let takes_value =
                        matches!(words[i].as_str(), "-s" | "--signal" | "-k" | "--kill-after");
                    i += if takes_value { 2 } else { 1 };
                }
                i += 1; // timeout 的期限，不是 sleep 的参数
            }
            word if word.contains('=') => i += 1,
            _ => return Some(i),
        }
    }
    None
}

fn duration(value: &str, suffixes: bool) -> Option<f64> {
    if value.eq_ignore_ascii_case("infinity") {
        return Some(f64::INFINITY);
    }
    let (number, factor) = if suffixes {
        match value.as_bytes().last()? {
            b's' => (&value[..value.len() - 1], 1000.0),
            b'm' => (&value[..value.len() - 1], 60_000.0),
            b'h' => (&value[..value.len() - 1], 3_600_000.0),
            b'd' => (&value[..value.len() - 1], 86_400_000.0),
            _ => (value, 1000.0),
        }
    } else {
        (value, 1000.0)
    };
    if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return None;
    }
    let value: f64 = number.parse().ok()?;
    (value >= 0.0).then_some(value * factor)
}

fn sleep_duration(words: &[String], platform: ShellPlatform) -> Option<u64> {
    let index = command_index(words)?;
    let name = &words[index];
    let args = &words[index + 1..];
    let ms = if platform == ShellPlatform::Windows {
        if !name.eq_ignore_ascii_case("sleep") && !name.eq_ignore_ascii_case("Start-Sleep") {
            return None;
        }
        match args {
            [value] => duration(value, false)?,
            [flag, value] if flag.eq_ignore_ascii_case("-Milliseconds") => {
                duration(value, false)? / 1000.0
            }
            [flag, value]
                if flag.eq_ignore_ascii_case("-Seconds") || flag.eq_ignore_ascii_case("-s") =>
            {
                duration(value, false)?
            }
            _ => return None,
        }
    } else {
        if name != "sleep" || args.is_empty() {
            return None;
        }
        args.iter()
            .filter(|s| s.as_str() != "--")
            .try_fold(0.0, |sum, arg| duration(arg, true).map(|value| sum + value))?
    };
    Some(ms.min(u64::MAX as f64) as u64)
}

pub(crate) fn reject_long_sleep(
    command: &str,
    background: bool,
    platform: ShellPlatform,
) -> Option<SleepRejection> {
    let segments = segments(command);
    let pure = !segments.is_empty()
        && segments
            .iter()
            .all(|segment| !segment.detached && sleep_duration(&segment.words, platform).is_some());
    if background && !pure {
        return None;
    }
    let segment = segments.iter().find_map(|segment| {
        if segment.detached {
            return None;
        }
        sleep_duration(&segment.words, platform)
            .filter(|ms| *ms >= THRESHOLD_MS)
            .map(|ms| (segment, ms))
    })?;
    let alternatives = if pure {
        vec![
            "纯 sleep 不执行实际工作；请做实际工作，或使用 TaskOutput 诊断已有任务并交回完成通知。",
        ]
    } else {
        vec![
            "等待已有任务：使用 TaskOutput 或完成通知，不要 sleep 轮询。",
            "合法延迟测试：将整个命令设为 run_in_background: true。",
        ]
    };
    Some(SleepRejection {
        code: if background {
            "background_pure_sleep"
        } else {
            "foreground_long_sleep"
        },
        started: false,
        threshold_ms: THRESHOLD_MS,
        detected_ms: segment.1,
        segment: segment.0.words.join(" "),
        alternatives,
    })
}

#[cfg(test)]
#[path = "foreground_sleep_test.rs"]
mod tests;
