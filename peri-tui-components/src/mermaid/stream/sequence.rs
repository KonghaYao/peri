//! 时序图语句扫描与识别。

use super::SequenceBlock;
use super::StatementScan;
use super::flow::strip_inline_comment;

// ── 时序图语句识别 ──────────────────────────────────────────────────────

pub(super) fn scan_sequence_statement(source: &str, start: usize, terminal: bool) -> StatementScan {
    if let Some(offset) = source[start..].find('\n') {
        let newline = start + offset;
        let text_end = newline
            .checked_sub(1)
            .filter(|&index| source.as_bytes().get(index) == Some(&b'\r'))
            .unwrap_or(newline);
        return StatementScan::Statement {
            start,
            text_end,
            consumed_end: newline + 1,
        };
    }
    if terminal {
        return StatementScan::Statement {
            start,
            text_end: source.len(),
            consumed_end: source.len(),
        };
    }
    StatementScan::None
}

pub(super) enum SequenceClass {
    Ignorable,
    Unknown,
    Header,
    ScopeOpen(SequenceBlock, String),
    Box,
    Continuation(&'static str),
    End,
    Directive,
    Message,
}

pub(super) fn classify_sequence(statement: &str, messages_started: bool) -> SequenceClass {
    let trimmed = statement.trim();
    if trimmed.is_empty() || trimmed.starts_with("%%") {
        return SequenceClass::Ignorable;
    }
    let cleaned = strip_inline_comment(trimmed).trim();
    if cleaned.is_empty() {
        return SequenceClass::Ignorable;
    }
    let lower = cleaned.to_ascii_lowercase();
    if lower.starts_with("sequencediagram") {
        return SequenceClass::Header;
    }
    let first = cleaned.split_whitespace().next().unwrap_or_default();
    let head = first.to_ascii_lowercase();
    match head.as_str() {
        "participant" | "actor" => {
            if has_rest(cleaned, first) {
                SequenceClass::Directive
            } else {
                SequenceClass::Unknown
            }
        }
        "autonumber" | "note" => SequenceClass::Directive,
        "activate" | "deactivate" => {
            if has_rest(cleaned, first) {
                SequenceClass::Directive
            } else {
                SequenceClass::Unknown
            }
        }
        "box" => {
            if messages_started {
                SequenceClass::Unknown
            } else {
                SequenceClass::Box
            }
        }
        "loop" => SequenceClass::ScopeOpen(SequenceBlock::Loop, rest_text(cleaned, first)),
        "alt" => SequenceClass::ScopeOpen(SequenceBlock::Alt, rest_text(cleaned, first)),
        "opt" => SequenceClass::ScopeOpen(SequenceBlock::Opt, rest_text(cleaned, first)),
        "par" => SequenceClass::ScopeOpen(SequenceBlock::Par, rest_text(cleaned, first)),
        "critical" => SequenceClass::ScopeOpen(SequenceBlock::Critical, rest_text(cleaned, first)),
        "break" => SequenceClass::ScopeOpen(SequenceBlock::Break, rest_text(cleaned, first)),
        "rect" => SequenceClass::ScopeOpen(SequenceBlock::Rect, rest_text(cleaned, first)),
        "else" => SequenceClass::Continuation("else"),
        "and" => SequenceClass::Continuation("and"),
        "option" => SequenceClass::Continuation("option"),
        "end" => {
            if has_rest(cleaned, first) {
                SequenceClass::Unknown
            } else {
                SequenceClass::End
            }
        }
        _ => {
            if parse_sequence_message(cleaned) {
                SequenceClass::Message
            } else {
                SequenceClass::Unknown
            }
        }
    }
}

fn has_rest(cleaned: &str, first: &str) -> bool {
    !cleaned[first.len()..].trim().is_empty()
}

fn rest_text(cleaned: &str, first: &str) -> String {
    cleaned[first.len()..].trim().to_string()
}

pub(super) fn is_sequence_message_directive(statement: &str) -> bool {
    let cleaned = strip_inline_comment(statement).trim();
    let first = cleaned.split_whitespace().next().unwrap_or_default();
    matches!(first.to_ascii_lowercase().as_str(), "participant" | "actor")
}

fn parse_sequence_message(statement: &str) -> bool {
    let Some((index, length)) = find_sequence_arrow(statement) else {
        return false;
    };
    let from = statement[..index].trim();
    if from.is_empty() {
        return false;
    }
    let rest = &statement[index + length..];
    let to_part = rest.split_once(':').map_or(rest, |(to, _)| to);
    let mut to = to_part.trim();
    if let Some(stripped) = to.strip_prefix(['+', '-']) {
        to = stripped.trim();
    }
    !to.is_empty()
}

fn find_sequence_arrow(statement: &str) -> Option<(usize, usize)> {
    const ARROWS: &[&str] = &["-->>", "-->", "->>", "->"];
    let mut quote = false;
    let mut escaped = false;
    let mut index = 0;
    while index < statement.len() {
        let character = statement[index..]
            .chars()
            .next()
            .expect("index stays on a UTF-8 boundary");
        if quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quote = false;
            }
            index += character.len_utf8();
            continue;
        }
        if character == '"' {
            quote = true;
            index += 1;
            continue;
        }
        for arrow in ARROWS {
            if statement[index..].starts_with(arrow) {
                return Some((index, arrow.len()));
            }
        }
        index += character.len_utf8();
    }
    None
}
