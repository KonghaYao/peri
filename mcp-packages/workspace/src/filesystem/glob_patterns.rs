//! glob 过滤器的语法规则：`{a,b}` 交替展开 + `glob::Pattern` 编译。
//!
//! Grep 的 `glob` 与 Glob 的 `pattern` 共用这一处实现。`glob` crate（0.3）没有
//! 交替语义——`{ts,tsx}` 会被当成字面量——所以推荐语法在这里先展开再编译。
//! 非法 pattern 一律返回诊断，由工具转成可行动的工具错误：静默丢弃过滤器会把
//! 「过滤后没有匹配」和「过滤器根本没生效」输出成同一个结果。

use glob::Pattern;

/// 交替展开后的 pattern 数上限。`{a,b}{c,d}…` 是指数级组合，超出即报错，
/// 不让一次调用编译出成千上万个 pattern。
const MAX_EXPANDED_PATTERNS: usize = 128;

/// 编译 glob pattern：展开 `{a,b}` 交替后逐个编译。
///
/// 语法错误（字符类/交替括号未闭合、展开超限）返回诊断字符串；调用方负责
/// 把它包装成带恢复指引的工具错误。
pub(crate) fn compile_glob_patterns(pattern: &str) -> Result<Vec<Pattern>, String> {
    let mut expanded = Vec::new();
    expand_alternates(pattern, &mut expanded)?;
    expanded
        .iter()
        .map(|candidate| {
            Pattern::new(candidate)
                .map_err(|error| format!("invalid glob pattern {candidate:?}: {error}"))
        })
        .collect()
}

/// 编译一组 glob 过滤器（`-g` 与 type 映射）：任一分支非法即返回其诊断。
pub(crate) fn compile_glob_filters(filters: &[String]) -> Result<Vec<Pattern>, String> {
    let mut patterns = Vec::new();
    for filter in filters {
        patterns.extend(compile_glob_patterns(filter)?);
    }
    Ok(patterns)
}

/// 把 pattern 中第一个 `{…}` 按顶层逗号展开成多个 pattern，递归处理嵌套与后续交替。
fn expand_alternates(pattern: &str, out: &mut Vec<String>) -> Result<(), String> {
    let Some(open) = pattern.find('{') else {
        if out.len() >= MAX_EXPANDED_PATTERNS {
            return Err(format!(
                "glob alternation expands past {MAX_EXPANDED_PATTERNS} patterns"
            ));
        }
        out.push(pattern.to_string());
        return Ok(());
    };
    let close = matching_brace(pattern, open).ok_or_else(|| {
        format!("unclosed `{{` in glob pattern {pattern:?}: close it or remove it")
    })?;
    let head = &pattern[..open];
    let tail = &pattern[close + 1..];
    for alternative in split_alternatives(&pattern[open + 1..close]) {
        expand_alternates(&format!("{head}{alternative}{tail}"), out)?;
    }
    Ok(())
}

/// 返回与 `open` 处 `{` 配对的 `}` 字节下标；未闭合返回 `None`。
fn matching_brace(pattern: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in pattern[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// 按顶层逗号切分交替体；嵌套 `{}` 内的逗号属于内层交替。
fn split_alternatives(inner: &str) -> Vec<&str> {
    let mut alternatives = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (offset, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                alternatives.push(&inner[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    alternatives.push(&inner[start..]);
    alternatives
}

#[cfg(test)]
#[path = "glob_patterns_test.rs"]
mod tests;
