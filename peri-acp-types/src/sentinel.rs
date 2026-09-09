//! Shared V1 grammar for Peri projection sentinel lines.

use std::collections::HashMap;

const PREFIX_V1: &str = "... [";
const SUFFIX_V1: &str = " 字符已省略] ...";

/// Sentinel occurrences keyed by their canonical body (without a line ending).
pub type ProjectionSentinelCountsV1 = HashMap<String, usize>;

/// Formats the canonical V1 sentinel body without a line ending.
pub fn format_projection_sentinel_v1(skipped_chars: usize) -> String {
    format!("{PREFIX_V1}{skipped_chars}{SUFFIX_V1}")
}

/// Counts complete V1 sentinel logical lines in `text`.
///
/// A line must contain exactly `... [` followed by one or more ASCII digits and
/// ` 字符已省略] ...`. LF, CRLF, and end-of-input terminate a line; a bare CR does
/// not. Digit sequences are deliberately not parsed as integers.
pub fn projection_sentinel_counts_v1(text: &str) -> ProjectionSentinelCountsV1 {
    let mut counts = ProjectionSentinelCountsV1::new();
    let bytes = text.as_bytes();
    let mut line_start = 0;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }

        let line_end = if index > line_start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        count_line(&text[line_start..line_end], &mut counts);
        line_start = index + 1;
    }

    if line_start < text.len() {
        count_line(&text[line_start..], &mut counts);
    }

    counts
}

/// Returns whether `post` contains any V1 sentinel lexeme more often than `pre`.
pub fn introduces_projection_sentinel_v1(pre: &str, post: &str) -> bool {
    let pre_counts = projection_sentinel_counts_v1(pre);
    projection_sentinel_counts_v1(post)
        .into_iter()
        .any(|(sentinel, count)| count > pre_counts.get(&sentinel).copied().unwrap_or(0))
}

fn count_line(line: &str, counts: &mut ProjectionSentinelCountsV1) {
    if !is_projection_sentinel_v1(line) {
        return;
    }

    *counts.entry(line.to_owned()).or_default() += 1;
}

fn is_projection_sentinel_v1(line: &str) -> bool {
    let Some(rest) = line.strip_prefix(PREFIX_V1) else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(SUFFIX_V1) else {
        return false;
    };

    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
#[path = "sentinel_test.rs"]
mod tests;
